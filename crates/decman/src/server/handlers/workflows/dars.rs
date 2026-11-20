//! Local DAR upload and the DAR distribution workflow.

use std::time::Duration;

use actix_web::{HttpRequest, HttpResponse, Responder, get, http::StatusCode, post, web};
use base64::{Engine, engine::general_purpose::STANDARD};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    db::schema::SchemaRead,
    error::Result,
    noise::{
        MAX_CHUNKED_TOTAL_SIZE, Message, MessageType, NoiseKeypair, parse_public_key,
        send_noise_message,
    },
    server::{
        AppState,
        middleware::require_admin,
        package_inventory::fetch_dar_for_package,
        types::{
            DarsInvitePayload, DarsRequest, DistributePackageRequest, ErrorResponse,
            SuccessResponse, WorkflowGuard, WorkflowInstance, WorkflowKind, WorkflowProgress,
            WorkflowResponse, WorkflowRole, WorkflowStatusResponse,
        },
    },
    utils,
    workflow::{self, DarsStep, contracts::DarFile},
};

use super::{
    onboarding::interpret_invite_reply,
    preflight::{format_incompatible_peers, preflight_incompatible_peers},
    runs::{kind_status, mark_run_completed, mark_run_failed, now_secs, register_and_persist},
};

// ============================================================================
// DARs Upload (Local)
// ============================================================================

/// Upload DAR files to the current node only (no distribution to peers)
#[utoipa::path(
    tag = "Workflows",
    request_body = DarsRequest,
    responses(
        (status = 200, description = "DARs uploaded to local node", body = SuccessResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 500, description = "Upload failed", body = ErrorResponse)
    )
)]
#[post("/dars/upload")]
pub async fn upload_dars_local(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<DarsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    match workflow::contracts::upload_dars(&data.config, &body.dar_files).await {
        Ok(()) => {
            tracing::info!(
                "Uploaded {} DAR file(s) to local node",
                body.dar_files.len()
            );
            HttpResponse::Ok().json(SuccessResponse { success: true })
        }
        Err(e) => {
            tracing::error!("Failed to upload DARs to local node: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to upload DARs: {e}"),
            })
        }
    }
}

// ============================================================================
// DARs Distribution Workflow
// ============================================================================

/// Distribute DARs across all participants via Noise protocol
#[utoipa::path(
    tag = "Workflows",
    request_body = DarsRequest,
    responses(
        (status = 202, description = "DARs distribution workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request (e.g. empty peer_ids)", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse),
        (status = 413, description = "The DAR set exceeds the size one run can carry", body = ErrorResponse)
    )
)]
#[post("/dars/distribute")]
pub async fn start_dars(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<DarsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    if body.peer_ids.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "peer_ids must contain at least one peer".to_string(),
        });
    }
    let body = body.into_inner();
    launch_dars(&data, body.dar_files, body.peer_ids).await
}

/// Distribute the DAR on this node that holds a package
///
/// The DAR is read from this participant, so the operator does not upload it
/// again. It then runs the same workflow as `/dars/distribute`: each peer
/// accepts or rejects the invitation as before.
#[utoipa::path(
    tag = "Workflows",
    request_body = DistributePackageRequest,
    responses(
        (status = 202, description = "DARs distribution workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request (e.g. empty peer_ids)", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No DAR on this node holds the package", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse),
        (status = 413, description = "The DAR exceeds the size one run can carry", body = ErrorResponse),
        (status = 500, description = "The DAR could not be read", body = ErrorResponse)
    )
)]
#[post("/dars/distribute-package")]
pub async fn start_dars_for_package(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<DistributePackageRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    if body.peer_ids.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "peer_ids must contain at least one peer".to_string(),
        });
    }
    let dar = match fetch_dar_for_package(&data.config, &body.package_id).await {
        Ok(Some(dar)) => dar,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: format!("No DAR on this node holds package {}", body.package_id),
            });
        }
        Err(e) => {
            tracing::error!(
                "Failed to read the DAR for package {}: {e:#}",
                body.package_id
            );
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to read the DAR: {e}"),
            });
        }
    };
    let body = body.into_inner();
    launch_dars(&data, vec![dar], body.peer_ids).await
}

/// Refuse a DAR set whose encoded `UploadDars` bundle exceeds `limit` bytes.
///
/// A peer checks the size only after it accepts the invitation, so the
/// coordinator checks it first. The bundle frames each file with its name and
/// two lengths, and those bytes count toward the limit.
fn dars_bundle_refusal(
    files: &[DarFile],
    limit: usize,
) -> std::result::Result<(), (StatusCode, String)> {
    let mut sizes = Vec::with_capacity(files.len());
    for file in files {
        let data = STANDARD.decode(&file.data).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!(
                    "DAR file {name} is not valid base64: {e}",
                    name = file.filename
                ),
            )
        })?;
        sizes.push((file.filename.as_str(), data.len()));
    }
    let Some(error) = bundle_size_refusal(&sizes, limit) else {
        return Ok(());
    };
    tracing::warn!(
        "Refused to distribute {count} DAR file(s): {error}",
        count = files.len()
    );
    Err((StatusCode::PAYLOAD_TOO_LARGE, error))
}

/// Why the bundle of `files`, each a name and a DAR length, does not fit in
/// `limit` bytes, or `None` when it fits.
///
/// A DAR that is over the limit alone can never be distributed, so the
/// refusal names it and does not suggest several runs.
fn bundle_size_refusal(files: &[(&str, usize)], limit: usize) -> Option<String> {
    // A size too large to count is over any limit.
    let size_of = |files: &[(&str, usize)]| {
        utils::encoded_files_len(files.iter().map(|(name, len)| (name.len(), *len)))
            .unwrap_or(usize::MAX)
    };
    let size = size_of(files);
    if size <= limit {
        return None;
    }
    let mib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    let limit_text = format!("{limit} bytes ({limit_mib:.1} MiB)", limit_mib = mib(limit));
    let over = |size: usize| {
        format!(
            "That is {over} bytes over the limit of {limit_text}.",
            over = size - limit
        )
    };
    let alone: Vec<(&str, usize)> = files
        .iter()
        .map(|file| (file.0, size_of(std::slice::from_ref(file))))
        .filter(|(_, size)| *size > limit)
        .collect();
    Some(match alone.as_slice() {
        [] => format!(
            "The DAR set is too large for one run. The files and their names take {size} bytes \
             ({size_mib:.1} MiB). {over} Send the files in several runs, each within the limit.",
            size_mib = mib(size),
            over = over(size),
        ),
        [(name, alone_size)] => format!(
            "{name} is too large to distribute, even on its own. The file and its name take \
             {alone_size} bytes ({size_mib:.1} MiB). {over} Upload it to each node separately \
             instead.",
            size_mib = mib(*alone_size),
            over = over(*alone_size),
        ),
        several => format!(
            "These DARs are each too large to distribute, even on their own: {names}. The limit \
             is {limit_text}. Upload them to each node separately instead.",
            names = several
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", "),
        ),
    })
}

/// Start a DARs distribution run: register it, invite the peers and run the
/// coordinator. The caller has checked the admin role and that `peer_ids` is
/// not empty.
async fn launch_dars(
    data: &web::Data<AppState>,
    dar_files: Vec<DarFile>,
    peer_ids: Vec<CantonId>,
) -> HttpResponse {
    if let Err((status, error)) = dars_bundle_refusal(&dar_files, MAX_CHUNKED_TOTAL_SIZE) {
        return HttpResponse::build(status).json(ErrorResponse { error });
    }

    // Create DARs config from request
    let timestamp = now_secs();
    let instance_name = format!("dars-distribute-{timestamp}");
    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::Dars,
        WorkflowRole::Coordinator,
    );
    let dars_state = &instance.http;
    let dars_config = workflow::DarsConfig {
        dar_files,
        instance_name: instance_name.clone(),
        peer_ids: peer_ids.clone(),
    };

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible = preflight_incompatible_peers(&data.config, &data.db, &peer_ids).await;
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    if let Err(resp) = register_and_persist(
        data,
        &instance,
        DarsStep::WaitingForPeers,
        &dars_config,
        &peer_ids,
        None,
    )
    .await
    {
        return resp;
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let dars_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    let last_seen = data.last_seen.clone();
    *dars_state.invited_peers.write().await = peer_ids.clone();
    let instance_for_task = instance_name.clone();

    // Acquire abort_handle, status, and error locks BEFORE spawning, then
    // flip all three together. Held simultaneously around the spawn so a
    // concurrent /dars/cancel either runs entirely before us (status !=
    // InProgress → 409 "no workflow in progress") or entirely after us
    // (status InProgress AND abort_handle Some → cancel proceeds). Without
    // this, a cancel arriving between the status flip and the abort_handle
    // assignment would observe "InProgress + None" and bail with the
    // spurious "still initializing" 409 — which leaves dars_state pinned
    // to InProgress and breaks the next /dars/distribute with a stale
    // 409. Holding `status.write()` across the spawn also blocks the
    // spawned task's terminal status write until our InProgress flip
    // lands, so a fast-failing task can't publish its terminal status
    // before we publish InProgress.
    let mut abort_guard = dars_state.abort_handle.lock().await;
    let mut status_guard = dars_state.status.write().await;
    let mut error_guard = dars_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send invites to selected peers before starting coordinator workflow
        let dar_filenames: Vec<String> = dars_config
            .dar_files
            .iter()
            .map(|f| f.filename.clone())
            .collect();
        // Pin the content, not just the name: the invitation the operator sees
        // carries the hash of the exact bytes the peers will be asked to upload
        // and vet. An empty hash list is the wire signal for "coordinator
        // predates the field", which makes peers fall back to checking
        // filenames only — so a DAR we cannot decode has to fail the run rather
        // than quietly downgrade every peer's check.
        let dar_hashes: std::result::Result<Vec<String>, _> = dars_config
            .dar_files
            .iter()
            .map(|f| {
                STANDARD
                    .decode(&f.data)
                    .map(|bytes| workflow::validation::hash_dar(&bytes))
            })
            .collect();
        let invite_result = match dar_hashes {
            Ok(dar_hashes) => {
                send_dars_invites(
                    &config,
                    &db,
                    &peer_ids,
                    &dar_filenames,
                    &dar_hashes,
                    &dars_config.instance_name,
                )
                .await
            }
            Err(e) => Err(anyhow::anyhow!(
                "Could not hash the DARs for the invitation: {e}"
            )),
        };
        if let Err(e) = invite_result {
            tracing::error!("Failed to send DARs invites: {e}");
            let mut status = dars_state_clone.status.write().await;
            let mut error = dars_state_clone.error.write().await;
            *status = WorkflowProgress::Failed;
            *error = Some(format!("Failed to send invites: {e}"));
            mark_run_failed(
                &db,
                kind,
                &instance_for_task,
                &format!("Failed to send invites: {e}"),
            )
            .await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            db.clone(),
            workflow::WorkflowType::Dars,
            None, // No onboarding config
            None, // No kick config
            None, // No contracts config
            Some(dars_config),
            None, // No add-party config
            None, // No change-threshold config
            None, // No rotate-key config
            None, // No auth
            last_seen,
            instance_for_coord,
        )
        .await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await (see kick/onboarding/contracts handlers above).
        match result {
            Ok(_) => {
                // The run row is marked terminal BEFORE the in-memory status
                // flips. The two are read by different callers — `/…/status`
                // reads this lock, while the "already has a workflow in flight"
                // guard scans the run rows — so flipping memory first opens a
                // window where the API reports a run finished and refuses the
                // next workflow for that party in the same breath. Doing the
                // durable write first can only report a finished run as still
                // running for an instant, which costs a poll rather than a
                // rejected request.
                mark_run_completed(&db, kind, &instance_for_task).await;
                {
                    let mut status = dars_state_clone.status.write().await;
                    *status = WorkflowProgress::Completed;
                }
                tracing::info!("DARs distribution workflow completed successfully");
            }
            Err(e) => {
                let msg = format!("{e}");
                // Durable row first, in-memory status second — same ordering as the
                // success path above, and for the same reason: a caller that reads
                // Failed from `/…/status` and retries must not be refused by the
                // in-flight guard, which scans the run rows.
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
                {
                    let mut status = dars_state_clone.status.write().await;
                    let mut error = dars_state_clone.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("DARs distribution workflow failed: {e}");
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = WorkflowProgress::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "DARs distribution workflow started".to_string(),
        instance_name,
    })
}

/// Get the current status of the DARs distribution workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "DARs distribution workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/dars/distribute/status")]
pub async fn get_dars_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Dars).await)
}

/// Send DARs invites to selected peers using Noise protocol
async fn send_dars_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    dar_filenames: &[String],
    dar_hashes: &[String],
    instance_name: &str,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let payload = DarsInvitePayload {
        dar_filenames: dar_filenames.to_vec(),
        dar_hashes: dar_hashes.to_vec(),
        // Carry the member set so the peer card shows the same participant
        // list the coordinator shows.
        participants: peer_ids.to_vec(),
        workflow_instance: Some(instance_name.to_string()),
    };
    let payload_bytes = serde_json::to_vec(&payload)?;
    let invite_message = Message::new(MessageType::InviteDars, payload_bytes);

    for peer_id in peer_ids {
        let peer = match network_config
            .peers
            .iter()
            .find(|p| &p.participant_id == peer_id)
        {
            Some(p) => p,
            None => {
                tracing::warn!("Skipping invite to {peer_id} - peer not found in network config");
                continue;
            }
        };

        if peer.public_key.is_empty() {
            tracing::warn!("Skipping invite to {peer_id} - no public key configured");
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!("Skipping invite to {peer_id} - invalid public key: {e}");
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending DARs invite to {peer_id} at {addr}:{port}",
            addr = peer.address,
            port = peer.port
        );

        match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &invite_message,
        )
        .await
        {
            Ok(response) => {
                interpret_invite_reply(peer_id, "DARs", &response)?;
            }
            Err(e) => {
                tracing::error!("Failed to send DARs invite to {peer_id}: {e}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use actix_web::{App, HttpMessage, test as actix_test};

    use crate::db::MIGRATOR;

    use super::super::test_support::{admin, pid, state_on};
    use super::*;

    fn dar(filename: &str, len: usize) -> DarFile {
        DarFile {
            filename: filename.to_string(),
            data: STANDARD.encode(vec![0u8; len]),
        }
    }

    /// Each file adds its name and 8 bytes of framing, and the set adds 4.
    /// Two DARs that fit alone but not together can go in two runs.
    #[test]
    fn a_set_above_the_limit_states_its_size_and_excess() -> Result {
        let refusal = dars_bundle_refusal(&[dar("a.dar", 60), dar("b.dar", 60)], 100);
        let Err((status, error)) = refusal else {
            anyhow::bail!("a 150-byte bundle must be refused at limit 100");
        };
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(error.contains("their names take 150 bytes"), "{error}");
        assert!(
            error.contains("50 bytes over the limit of 100 bytes"),
            "{error}"
        );
        assert!(error.contains("several runs"), "{error}");
        Ok(())
    }

    /// No split helps a DAR that is over the limit alone, so the refusal
    /// names it and does not suggest several runs.
    #[test]
    fn a_dar_over_the_limit_alone_is_named_without_advice_to_split() -> Result {
        let refusal = dars_bundle_refusal(&[dar("a.dar", 10), dar("big.dar", 120)], 100);
        let Err((status, error)) = refusal else {
            anyhow::bail!("a DAR over the limit alone must be refused");
        };
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            error.starts_with("big.dar is too large to distribute"),
            "{error}"
        );
        assert!(error.contains("its name take 139 bytes"), "{error}");
        assert!(
            error.contains("39 bytes over the limit of 100 bytes"),
            "{error}"
        );
        assert!(!error.contains("several runs"), "{error}");
        Ok(())
    }

    #[test]
    fn every_dar_over_the_limit_alone_is_named() {
        let refusal = bundle_size_refusal(&[("x.dar", 120), ("a.dar", 1), ("y.dar", 130)], 100)
            .unwrap_or_default();
        assert!(refusal.contains(": x.dar, y.dar."), "{refusal}");
        assert!(!refusal.contains("several runs"), "{refusal}");
    }

    /// A size too large to add up is refused, not wrapped to a small one.
    #[test]
    fn a_size_that_overflows_is_refused() {
        assert!(bundle_size_refusal(&[("a.dar", usize::MAX)], 100).is_some());
        assert!(bundle_size_refusal(&[("a.dar", usize::MAX - 20), ("b.dar", 10)], 100).is_some());
    }

    #[test]
    fn framing_bytes_count_toward_the_limit() {
        let refusal = dars_bundle_refusal(&[dar("a.dar", 60), dar("b.dar", 40)], 100);
        assert!(
            matches!(refusal, Err((StatusCode::PAYLOAD_TOO_LARGE, _))),
            "100 DAR bytes plus framing must exceed limit 100: {refusal:?}"
        );
    }

    #[test]
    fn a_bundle_at_the_limit_passes() {
        assert_eq!(dars_bundle_refusal(&[dar("a.dar", 83)], 100), Ok(()));
    }

    #[test]
    fn an_undecodable_dar_is_a_bad_request() -> Result {
        let files = [
            dar("good.dar", 10),
            DarFile {
                filename: "broken.dar".to_string(),
                data: "not base64!".to_string(),
            },
        ];
        let Err((status, error)) = dars_bundle_refusal(&files, 100) else {
            anyhow::bail!("a DAR that is not base64 must be refused");
        };
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.contains("broken.dar"), "{error}");
        Ok(())
    }

    /// One DAR the size of the limit fails only through its framing. The run
    /// is refused before it is registered, so no peer is invited.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_oversized_distribute_sends_no_invitation(pool: SqlitePool) -> Result {
        let data = state_on(pool.clone()).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(web::JsonConfig::default().limit(100 * 1024 * 1024))
                .app_data(data.clone())
                .service(start_dars),
        )
        .await;
        let request = actix_test::TestRequest::post()
            .uri("/dars/distribute")
            .set_json(serde_json::json!({
                "dar_files": [dar("big.dar", MAX_CHUNKED_TOTAL_SIZE)],
                "peer_ids": [pid(2)?],
            }))
            .to_request();
        request.extensions_mut().insert(admin());

        let response = actix_test::call_service(&app, request).await;

        let status = response.status();
        let body = actix_test::read_body(response).await;
        let body = String::from_utf8_lossy(&body);
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
        assert!(
            body.contains("big.dar is too large to distribute"),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                "19 bytes over the limit of {MAX_CHUNKED_TOTAL_SIZE} bytes"
            )),
            "{body}"
        );
        assert!(
            pool.get_workflow_runs_by_kind(WorkflowKind::Dars)
                .await?
                .is_empty()
        );
        assert!(data.workflows.snapshot().is_empty());
        Ok(())
    }

    /// Three DARs whose `UploadDars` bundle is `bundle_len` bytes.
    fn three_dars_of_bundle(bundle_len: usize) -> Vec<DarFile> {
        // A 4-byte count, then two 4-byte lengths and a 5-byte name per file.
        let data_len = bundle_len - 4 - 3 * (8 + 5);
        vec![
            dar("a.dar", data_len / 3),
            dar("b.dar", data_len / 3),
            dar("c.dar", data_len - 2 * (data_len / 3)),
        ]
    }

    /// The bundle the peer receives, built the way the coordinator builds it.
    fn bundle_of(files: &[DarFile]) -> anyhow::Result<Vec<u8>> {
        let decoded = files
            .iter()
            .map(|file| Ok((file.filename.clone(), STANDARD.decode(&file.data)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(utils::encode_files(&decoded))
    }

    /// The coordinator must refuse exactly the sets a peer would refuse. A
    /// stricter check blocks runs that would work. A looser one lets the peer
    /// fail after it accepts, which is the fault this check prevents.
    #[test]
    fn the_coordinator_refuses_exactly_what_a_peer_refuses() -> anyhow::Result<()> {
        use crate::noise::{CHUNK_SIZE, check_chunked_metadata};

        for bundle_len in [MAX_CHUNKED_TOTAL_SIZE, MAX_CHUNKED_TOTAL_SIZE + 1] {
            let files = three_dars_of_bundle(bundle_len);
            let bundle = bundle_of(&files)?;
            assert_eq!(bundle.len(), bundle_len);
            let peer = check_chunked_metadata(bundle.len(), bundle.len().div_ceil(CHUNK_SIZE));
            let coordinator = dars_bundle_refusal(&files, MAX_CHUNKED_TOTAL_SIZE);
            assert_eq!(
                coordinator.is_ok(),
                peer.is_ok(),
                "bundle of {bundle_len} bytes: coordinator {coordinator:?}, peer {peer:?}"
            );
        }
        Ok(())
    }

    /// Issue #616: many DARs, each well below the limit, that exceed it
    /// together. The refusal states the exact size of the bundle a peer
    /// would have received.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn many_small_dars_above_the_limit_together_are_refused(pool: SqlitePool) -> Result {
        let files: Vec<DarFile> = (0..9)
            .map(|n| dar(&format!("package-{n}.dar"), 2 * 1024 * 1024))
            .collect();
        let size = bundle_of(&files)?.len();
        let data = state_on(pool.clone()).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(web::JsonConfig::default().limit(100 * 1024 * 1024))
                .app_data(data.clone())
                .service(start_dars),
        )
        .await;
        let request = actix_test::TestRequest::post()
            .uri("/dars/distribute")
            .set_json(serde_json::json!({
                "dar_files": files,
                "peer_ids": [pid(2)?],
            }))
            .to_request();
        request.extensions_mut().insert(admin());

        let response = actix_test::call_service(&app, request).await;

        let status = response.status();
        let body = actix_test::read_body(response).await;
        let body = String::from_utf8_lossy(&body);
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
        assert!(
            body.contains(&format!("their names take {size} bytes")),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                "{over} bytes over the limit of {MAX_CHUNKED_TOTAL_SIZE} bytes",
                over = size - MAX_CHUNKED_TOTAL_SIZE
            )),
            "{body}"
        );
        assert!(body.contains("several runs"), "{body}");
        assert!(
            pool.get_workflow_runs_by_kind(WorkflowKind::Dars)
                .await?
                .is_empty()
        );
        assert!(data.workflows.snapshot().is_empty());
        Ok(())
    }
}
