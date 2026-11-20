//! Local DAR upload and the DAR distribution workflow.

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use base64::{Engine, engine::general_purpose::STANDARD};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    noise::{MAX_CHUNKED_TOTAL_SIZE, Message, MessageType},
    server::{
        AppState,
        error::ApiError,
        middleware::require_admin,
        package_inventory::fetch_dar_for_package,
        types::{
            DarsInvitePayload, DarsRequest, DistributePackageRequest, ErrorResponse,
            SuccessResponse, WorkflowInstance, WorkflowKind, WorkflowProgress, WorkflowResponse,
            WorkflowRole, WorkflowStatusResponse,
        },
    },
    utils,
    workflow::{self, DarsStep, contracts::DarFile},
};

use super::{
    invites::{InviteLabels, Recipients, send_invites},
    launch::{CoordinatorRequest, StatusOrder, launch_coordinator},
    preflight::{format_incompatible_peers, preflight_incompatible_peers},
    runs::{kind_status, now_secs, register_and_persist},
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
            ApiError::Internal(format!("Failed to upload DARs: {e}")).into_response()
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
        return ApiError::BadRequest("peer_ids must contain at least one peer".to_string())
            .into_response();
    }
    let body = body.into_inner();
    launch_dars(&data, body.dar_files, body.peer_ids)
        .await
        .unwrap_or_else(ApiError::into_response)
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
    start_dars_for_package_run(&data, body.into_inner())
        .await
        .unwrap_or_else(ApiError::into_response)
}

/// Read the DAR that holds the requested package, then distribute it.
async fn start_dars_for_package_run(
    data: &web::Data<AppState>,
    body: DistributePackageRequest,
) -> std::result::Result<HttpResponse, ApiError> {
    if body.peer_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "peer_ids must contain at least one peer".to_string(),
        ));
    }
    let dar = match fetch_dar_for_package(&data.config, &body.package_id).await {
        Ok(Some(dar)) => dar,
        Ok(None) => {
            return Err(ApiError::NotFound(format!(
                "No DAR on this node holds package {}",
                body.package_id
            )));
        }
        Err(e) => {
            tracing::error!(
                "Failed to read the DAR for package {}: {e:#}",
                body.package_id
            );
            return Err(ApiError::Internal(format!("Failed to read the DAR: {e}")));
        }
    };
    launch_dars(data, vec![dar], body.peer_ids).await
}

/// Refuse a DAR set whose encoded `UploadDars` bundle exceeds `limit` bytes.
///
/// A peer checks the size only after it accepts the invitation, so the
/// coordinator checks it first. The bundle frames each file with its name and
/// two lengths, and those bytes count toward the limit.
fn dars_bundle_refusal(files: &[DarFile], limit: usize) -> std::result::Result<(), ApiError> {
    let mut sizes = Vec::with_capacity(files.len());
    for file in files {
        let data = STANDARD.decode(&file.data).map_err(|e| {
            ApiError::BadRequest(format!(
                "DAR file {name} is not valid base64: {e}",
                name = file.filename
            ))
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
    Err(ApiError::PayloadTooLarge(error))
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
) -> std::result::Result<HttpResponse, ApiError> {
    dars_bundle_refusal(&dar_files, MAX_CHUNKED_TOTAL_SIZE)?;

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
        return Err(ApiError::Conflict(format_incompatible_peers(&incompatible)));
    }

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    register_and_persist(
        data,
        &instance,
        DarsStep::WaitingForPeers,
        &dars_config,
        &peer_ids,
        None,
    )
    .await?;

    *dars_state.invited_peers.write().await = peer_ids.clone();

    // Send invites to selected peers before starting coordinator workflow
    let invites = {
        let config = data.config.clone();
        let db = data.db.clone();
        let dars_config = dars_config.clone();
        Box::pin(async move {
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
            match dar_hashes {
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
            }
        })
    };
    launch_coordinator(
        data,
        &instance,
        CoordinatorRequest::Dars(dars_config),
        None,
        invites,
        StatusOrder::RowFirstHoldingLocksOnInviteFailure,
        None,
    )
    .await;

    Ok(HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "DARs distribution workflow started".to_string(),
        instance_name,
    }))
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

/// The DARs invite. It carries the member set, so the peer card shows the
/// same participant list the coordinator shows.
pub(super) fn dars_invite(
    peer_ids: &[CantonId],
    dar_filenames: &[String],
    dar_hashes: &[String],
    instance_name: &str,
) -> Result<Message> {
    let payload = DarsInvitePayload {
        dar_filenames: dar_filenames.to_vec(),
        dar_hashes: dar_hashes.to_vec(),
        participants: peer_ids.to_vec(),
        workflow_instance: Some(instance_name.to_string()),
    };
    let payload_bytes = serde_json::to_vec(&payload)?;
    Ok(Message::new(MessageType::InviteDars, payload_bytes))
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
    send_invites(
        config,
        db,
        Recipients::Listed(peer_ids),
        InviteLabels {
            kind: "DARs",
            skip: "",
            failed: "DARs ",
            address: true,
            empty_key_warning: true,
        },
        || dars_invite(peer_ids, dar_filenames, dar_hashes, instance_name),
    )
    .await
}

#[cfg(test)]
mod tests {
    use actix_web::{App, HttpMessage, http::StatusCode, test as actix_test};

    use crate::db::{MIGRATOR, schema::SchemaRead};

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
        let Err(ApiError::PayloadTooLarge(error)) = refusal else {
            anyhow::bail!("a 150-byte bundle must be refused at limit 100");
        };
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
        let Err(ApiError::PayloadTooLarge(error)) = refusal else {
            anyhow::bail!("a DAR over the limit alone must be refused");
        };
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
            matches!(refusal, Err(ApiError::PayloadTooLarge(_))),
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
        let Err(ApiError::BadRequest(error)) = dars_bundle_refusal(&files, 100) else {
            anyhow::bail!("a DAR that is not base64 must be refused");
        };
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

#[cfg(test)]
mod dar_handler_tests {
    //! Pins every answer the DAR upload and distribution endpoints give
    //! before they start a run: the status code and the exact JSON body.

    use actix_web::http::StatusCode;
    use serde_json::json;

    use super::super::{
        preflight::{format_incompatible_peers, preflight_incompatible_peers},
        test_support::{
            Answer, TestNode, admin, answer_of, break_table, closed_port, insert_peers, peer, pid,
            post_as, post_as_admin, test_node,
        },
    };
    use super::*;
    use crate::db::MIGRATOR;

    type Result<T = ()> = anyhow::Result<T>;

    /// This node is `pid(1)`, and its peer `pid(2)` has no usable Noise key
    /// unless `reachable_keys`.
    async fn node_with_a_peer(pool: SqlitePool, reachable_keys: bool) -> Result<TestNode> {
        insert_peers(&pool, &[peer(&pid(2)?, closed_port()?, reachable_keys)]).await?;
        test_node(pool, &pid(1)?).await
    }

    fn dar_file() -> DarFile {
        DarFile {
            filename: "app.dar".to_string(),
            data: STANDARD.encode(b"dar bytes"),
        }
    }

    fn distribute(peer_ids: &[CantonId]) -> serde_json::Value {
        json!({
            "dar_files": [dar_file()],
            "peer_ids": peer_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        })
    }

    fn distribute_package(peer_ids: &[CantonId]) -> serde_json::Value {
        json!({
            "package_id": "0000000000000000000000000000000000000000000000000000000000000001",
            "peer_ids": peer_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        })
    }

    async fn post_distribute(node: &TestNode, request: serde_json::Value) -> Answer {
        post_as_admin(&node.data, start_dars, "/dars/distribute", request).await
    }

    async fn post_distribute_package(node: &TestNode, request: serde_json::Value) -> Answer {
        post_as_admin(
            &node.data,
            start_dars_for_package,
            "/dars/distribute-package",
            request,
        )
        .await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn anonymous_callers_are_unauthorized(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool, false).await?;
        let peers = [pid(2)?];
        post_as(
            &node.data,
            upload_dars_local,
            "/dars/upload",
            distribute(&peers),
            None,
        )
        .await
        .assert_error(StatusCode::UNAUTHORIZED, "authentication required");
        post_as(
            &node.data,
            start_dars,
            "/dars/distribute",
            distribute(&peers),
            None,
        )
        .await
        .assert_error(StatusCode::UNAUTHORIZED, "authentication required");
        post_as(
            &node.data,
            start_dars_for_package,
            "/dars/distribute-package",
            distribute_package(&peers),
            None,
        )
        .await
        .assert_error(StatusCode::UNAUTHORIZED, "authentication required");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_empty_local_upload_succeeds(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool, false).await?;
        let answer = post_as_admin(
            &node.data,
            upload_dars_local,
            "/dars/upload",
            json!({ "dar_files": [] }),
        )
        .await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        assert_eq!(answer.body, r#"{"success":true}"#);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_local_upload_answers_500(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool, false).await?;
        let error = workflow::contracts::upload_dars(&node.data.config, &[dar_file()])
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("a closed admin port must refuse the upload"))?;
        post_as_admin(
            &node.data,
            upload_dars_local,
            "/dars/upload",
            distribute(&[]),
        )
        .await
        .assert_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to upload DARs: {error}"),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_distribution_needs_a_peer(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool, false).await?;
        post_distribute(&node, distribute(&[])).await.assert_error(
            StatusCode::BAD_REQUEST,
            "peer_ids must contain at least one peer",
        );
        post_distribute_package(&node, distribute_package(&[]))
            .await
            .assert_error(
                StatusCode::BAD_REQUEST,
                "peer_ids must contain at least one peer",
            );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_dar_read_answers_500(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool, false).await?;
        let error = fetch_dar_for_package(
            &node.data.config,
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .await
        .err()
        .ok_or_else(|| anyhow::anyhow!("a closed admin port must refuse the read"))?;
        post_distribute_package(&node, distribute_package(&[pid(2)?]))
            .await
            .assert_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Failed to read the DAR: {error}"),
            );
        Ok(())
    }

    /// POST a distribution of `dar_files` to `pid(2)` with a body limit that
    /// lets a DAR over the run limit through to the handler.
    async fn post_large_distribute(node: &TestNode, dar_files: &[DarFile]) -> Result<Answer> {
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::JsonConfig::default().limit(100 * 1024 * 1024))
                .app_data(node.data.clone())
                .service(start_dars),
        )
        .await;
        let request = actix_web::test::TestRequest::post()
            .uri("/dars/distribute")
            .set_json(json!({ "dar_files": dar_files, "peer_ids": [pid(2)?.to_string()] }))
            .to_request();
        actix_web::HttpMessage::extensions_mut(&request).insert(admin());
        Ok(answer_of(actix_web::test::call_service(&app, request).await).await)
    }

    fn zero_dar(filename: &str, len: usize) -> DarFile {
        DarFile {
            filename: filename.to_string(),
            data: STANDARD.encode(vec![0u8; len]),
        }
    }

    fn mib(bytes: usize) -> f64 {
        bytes as f64 / (1024.0 * 1024.0)
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_dar_that_is_not_base64_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool, false).await?;
        let error = STANDARD
            .decode("not base64!")
            .err()
            .ok_or_else(|| anyhow::anyhow!("the text must not decode"))?;
        let request = json!({
            "dar_files": [{ "filename": "broken.dar", "data": "not base64!" }],
            "peer_ids": [pid(2)?.to_string()],
        });
        post_distribute(&node, request).await.assert_error(
            StatusCode::BAD_REQUEST,
            &format!("DAR file broken.dar is not valid base64: {error}"),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_dar_over_the_run_limit_alone_is_too_large(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool.clone(), false).await?;
        let limit = MAX_CHUNKED_TOTAL_SIZE;
        let size = limit + 19;
        post_large_distribute(&node, &[zero_dar("big.dar", limit)])
            .await?
            .assert_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                &format!(
                    "big.dar is too large to distribute, even on its own. The file and its name \
                     take {size} bytes ({size_mib:.1} MiB). That is 19 bytes over the limit of \
                     {limit} bytes ({limit_mib:.1} MiB). Upload it to each node separately \
                     instead.",
                    size_mib = mib(size),
                    limit_mib = mib(limit),
                ),
            );
        assert!(node.data.workflows.snapshot().is_empty());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_dar_set_over_the_run_limit_is_too_large(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool, false).await?;
        let limit = MAX_CHUNKED_TOTAL_SIZE;
        let half = limit / 2;
        // A 4-byte count, then two lengths and a 5-byte name per file.
        let size = 4 + 2 * (8 + 5 + half);
        post_large_distribute(&node, &[zero_dar("a.dar", half), zero_dar("b.dar", half)])
            .await?
            .assert_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                &format!(
                    "The DAR set is too large for one run. The files and their names take {size} \
                     bytes ({size_mib:.1} MiB). That is {over} bytes over the limit of {limit} \
                     bytes ({limit_mib:.1} MiB). Send the files in several runs, each within the \
                     limit.",
                    size_mib = mib(size),
                    over = size - limit,
                    limit_mib = mib(limit),
                ),
            );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unverifiable_invitee_is_a_conflict(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool.clone(), true).await?;
        let incompatible = preflight_incompatible_peers(&node.data.config, &pool, &[pid(2)?]).await;
        assert_eq!(incompatible.len(), 1, "{incompatible:?}");
        post_distribute(&node, distribute(&[pid(2)?]))
            .await
            .assert_error(
                StatusCode::CONFLICT,
                &format_incompatible_peers(&incompatible),
            );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_run_write_is_a_conflict_and_leaves_no_run(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool.clone(), false).await?;
        break_table(&pool, "workflow_runs").await?;
        let answer = post_distribute(&node, distribute(&[pid(2)?])).await;
        assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.starts_with("Failed to start DARs workflow: ") && error.contains("workflow_runs"),
            "{error}"
        );
        assert_eq!(body.as_object().map(|o| o.len()), Some(1), "{body}");
        assert!(node.data.workflows.snapshot().is_empty());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_valid_distribution_is_accepted(pool: SqlitePool) -> Result {
        let node = node_with_a_peer(pool, false).await?;
        let answer = post_distribute(&node, distribute(&[pid(2)?])).await;
        assert_eq!(answer.status, StatusCode::ACCEPTED, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let instance = body["instance_name"].as_str().unwrap_or_default();
        assert!(instance.starts_with("dars-distribute-"), "{body}");
        assert_eq!(
            answer.body,
            format!(
                r#"{{"status":"inprogress","message":"DARs distribution workflow started","instance_name":"{instance}"}}"#
            )
        );
        Ok(())
    }
}
