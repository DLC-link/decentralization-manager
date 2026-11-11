//! Local DAR upload and the DAR distribution workflow.

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use base64::{Engine, engine::general_purpose::STANDARD};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
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
    workflow::{self, DarsStep, contracts::DarFile},
};

use super::{
    launch::{CoordinatorRequest, StatusOrder, launch_coordinator},
    onboarding::interpret_invite_reply,
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
        (status = 409, description = "Workflow already in progress", body = ErrorResponse)
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

/// Start a DARs distribution run: register it, invite the peers and run the
/// coordinator. The caller has checked the admin role and that `peer_ids` is
/// not empty.
async fn launch_dars(
    data: &web::Data<AppState>,
    dar_files: Vec<DarFile>,
    peer_ids: Vec<CantonId>,
) -> std::result::Result<HttpResponse, ApiError> {
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
mod dar_handler_tests {
    //! Pins every answer the DAR upload and distribution endpoints give
    //! before they start a run: the status code and the exact JSON body.

    use actix_web::http::StatusCode;
    use serde_json::json;

    use super::super::{
        preflight::{format_incompatible_peers, preflight_incompatible_peers},
        test_support::{
            Answer, TestNode, break_table, closed_port, insert_peers, peer, pid, post_as,
            post_as_admin, test_node,
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
