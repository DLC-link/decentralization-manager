use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use sqlx::SqlitePool;

use super::parties::{
    fetch_decentralized_parties, resolve_owner_keys_from_peers, store_parties_to_db,
};
use crate::{
    config::{NetworkConfig, NodeConfig},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    participant_id::CantonId,
    server::{
        AppState,
        middleware::require_admin,
        respawn_coordinator,
        types::{
            ContractsRequest, DarsInvitePayload, DarsRequest, ErrorResponse, HttpWorkflowState,
            KickRequest, KickResponse, KickStatus, ListenerPauseGuard, MessageResponse,
            MissingEdgeKind, MissingPeerEdge, OnboardingInvitePayload, OnboardingMeshErrorResponse,
            OnboardingRequest, OnboardingResponse, OnboardingStatus, SuccessResponse, WorkflowKind,
            WorkflowProgress, WorkflowResponse, WorkflowRole, WorkflowRun, WorkflowRunsResponse,
            WorkflowStatusResponse,
        },
    },
    workflow::{self, ContractsStep, DarsStep, KickStep, OnboardingStep, state::WorkflowStep},
};

// ============================================================================
// Workflow State Types
// ============================================================================

/// State for tracking kick workflow
pub type KickWorkflowState = HttpWorkflowState<KickStatus>;

/// State for tracking onboarding workflow
pub type OnboardingWorkflowState = HttpWorkflowState<OnboardingStatus>;

/// State for tracking contracts workflow
pub type ContractsWorkflowState = HttpWorkflowState<WorkflowProgress>;

/// State for tracking DARs distribution workflow
pub type DarsWorkflowState = HttpWorkflowState<WorkflowProgress>;

// ============================================================================
// Workflow run persistence helpers
// ============================================================================

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Insert the coordinator-side `workflow_runs` row for a freshly-started run.
/// The partial unique index enforces "one InProgress run per (kind, role)" so a
/// duplicate insert here surfaces as the right error to bubble back as 409.
async fn insert_coordinator_run<S, C>(
    db: &SqlitePool,
    instance_name: &str,
    kind: WorkflowKind,
    initial_step: S,
    config: &C,
    invitees: &[CantonId],
    dec_party_id: Option<CantonId>,
) -> Result
where
    S: WorkflowStep,
    C: serde::Serialize,
{
    let now = now_secs();
    let run = WorkflowRun {
        instance_name: instance_name.to_string(),
        kind,
        role: WorkflowRole::Coordinator,
        status: WorkflowProgress::InProgress,
        current_step: initial_step.step_name().to_string(),
        step_index: initial_step.step_index(),
        step_total: S::step_total(),
        config_json: serde_json::to_string(config)
            .map_err(|e| anyhow::anyhow!("encode workflow config: {e}"))?,
        coordinator_pubkey: None,
        coordinator_name: None,
        expected_peers: invitees.to_vec(),
        completed_peers: Vec::new(),
        dec_party_id,
        error: None,
        dismissed: false,
        created_at: now,
        updated_at: now,
    };

    let mut tx = db.begin_transaction().await?;
    tx.upsert_workflow_run(&run).await?;
    Commitable::commit(tx).await
}

/// Flip the persisted run's status to Completed. Errors are logged and
/// swallowed — the spawned task can't usefully react to a DB error here.
async fn mark_run_completed(db: &SqlitePool, instance_name: &str) {
    if let Err(e) = mark_run_status(db, instance_name, WorkflowProgress::Completed, None).await {
        tracing::warn!("Failed to mark run {instance_name} completed: {e:#}");
    }
}

/// Flip the persisted run's status to Failed with a message.
async fn mark_run_failed(db: &SqlitePool, instance_name: &str, error: &str) {
    if let Err(e) = mark_run_status(
        db,
        instance_name,
        WorkflowProgress::Failed,
        Some(error.to_string()),
    )
    .await
    {
        tracing::warn!("Failed to mark run {instance_name} failed: {e:#}");
    }
}

async fn mark_run_status(
    db: &SqlitePool,
    instance_name: &str,
    status: WorkflowProgress,
    error: Option<String>,
) -> Result {
    let mut tx = db.begin_transaction().await?;
    tx.set_workflow_run_status(instance_name, status, error.as_deref(), now_secs())
        .await?;
    Commitable::commit(tx).await
}

// ============================================================================
// Kick Workflow
// ============================================================================

/// Start a kick workflow to remove a participant from a decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = KickRequest,
    responses(
        (status = 202, description = "Kick workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress, or owner key not yet resolved for the target participant", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/kick")]
pub async fn start_kick(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    kick_state: web::Data<Arc<KickWorkflowState>>,
    body: web::Json<KickRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    tracing::info!(
        "Kick request received: party={}, participant_to_kick={}, threshold={}",
        body.decentralized_party_id,
        body.participant_id,
        body.new_threshold
    );

    // KickRequest's CantonId fields validate during deserialization, so by
    // the time we get here both ids are well-formed.
    let decentralized_party_id = body.decentralized_party_id.clone();
    let participant_id = body.participant_id.clone();

    // Prevent kicking ourselves
    if participant_id == *data.config.participant_id() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "Cannot kick yourself".to_string(),
        });
    }

    // Derive the namespace fingerprint from the cache. Server-side
    // derivation removes a redundant client field and turns empty-prefill
    // into a clear server error rather than a silent invalid request.
    let namespace_fingerprint = match data
        .db
        .get_dec_party_participant_owner_key(&decentralized_party_id, &participant_id.to_string())
        .await
    {
        Ok(Some(key)) => key,
        Ok(None) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!(
                    "Participant {participant_id} is not present in cached \
                     decentralized party {decentralized_party_id}, or its \
                     owner key has not yet been resolved. Try refreshing \
                     /decentralized-parties first."
                ),
            });
        }
        Err(e) => {
            tracing::error!("DB lookup for owner_key failed: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to look up owner key".to_string(),
            });
        }
    };

    // Check if a kick is already in progress
    {
        let status = kick_state.status.read().await;
        if *status == KickStatus::InProgress {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "A kick workflow is already in progress".to_string(),
            });
        }
    }

    // Load peers once: we use the count to bound `new_threshold` per the
    // audit finding, then filter into `invitees`. A DB error here is fatal
    // — we can't safely proceed without knowing how many signers remain.
    let peers = match data.db.get_all_peers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to load peers for kick: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load peers".to_string(),
            });
        }
    };

    // Validate `new_threshold` before persisting anything. Negative or zero
    // values corrupt topology submission: DNS `authorize()` accepts a bare
    // i32 while the subsequent P2P proposal converts via `try_into()` to
    // u32 and fails partway, leaving the DNS write committed with no
    // rollback. The upper bound is the post-kick member count — there
    // must be at least as many remaining signers as the threshold needs.
    let post_kick_member_count = peers.len() as i32 - 1;
    if body.new_threshold < 1 || body.new_threshold > post_kick_member_count {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "new_threshold must be between 1 and {post_kick_member_count} \
                 (peer count {n}, minus the participant being kicked); got {got}",
                n = peers.len(),
                got = body.new_threshold,
            ),
        });
    }

    // Compute peers we're going to invite — every peer except self + the kicked participant.
    // Done before the InProgress flip so a concurrent /kick/cancel cannot observe
    // InProgress while we're still preparing.
    let invitees: Vec<CantonId> = peers
        .into_iter()
        .map(|p| p.participant_id)
        .filter(|p| p != data.config.participant_id() && p != &participant_id)
        .collect();

    // Compute instance name + config up-front so the persisted workflow_runs row
    // can carry the same identifier the coordinator task will use.
    let timestamp = now_secs();
    let instance_name = format!("{}-kick-{timestamp}", decentralized_party_id.prefix);
    let kick_config = workflow::KickConfig::new(
        decentralized_party_id.clone(),
        participant_id.clone(),
        namespace_fingerprint,
        body.new_threshold,
        instance_name.clone(),
    );

    // Insert the workflow_runs row BEFORE flipping in-memory status. If the
    // insert fails (e.g. partial-unique-index says another kick is already in
    // flight), bubble that out as a 409 — same semantics as the existing
    // in-memory check just upgraded to durable storage.
    if let Err(e) = insert_coordinator_run(
        &data.db,
        &instance_name,
        WorkflowKind::Kick,
        KickStep::WaitingForPeers,
        &kick_config,
        &invitees,
        Some(decentralized_party_id.clone()),
    )
    .await
    {
        tracing::warn!("Failed to persist kick workflow run: {e:#}");
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to start kick workflow: {e}"),
        });
    }

    *kick_state.invited_peers.write().await = invitees;

    // Spawn the kick workflow in the background
    let config = data.config.clone();
    let db = data.db.clone();
    let kick_state_clone = kick_state.get_ref().clone();
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();
    let instance_for_task = instance_name.clone();

    // See start_dars below for the rationale: abort_handle, status, and error
    // are flipped under simultaneously-held locks so a concurrent /kick/cancel
    // can never observe "status=InProgress + abort_handle=None" and bail.
    let mut abort_guard = kick_state.abort_handle.lock().await;
    let mut status_guard = kick_state.status.write().await;
    let mut error_guard = kick_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send kick invites to all peers before starting coordinator workflow
        let invite_result = send_kick_invites(&config, &db, &participant_id).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send kick invites: {e}");
            guard.resume().await;
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = kick_state_clone.status.write().await;
                let mut error = kick_state_clone.error.write().await;
                *status = KickStatus::Failed;
                *error = Some(msg.clone());
            }
            mark_run_failed(&db, &instance_for_task, &msg).await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            db.clone(),
            workflow::WorkflowType::Kick,
            None, // No onboarding config
            Some(kick_config),
            None, // No contracts config
            None, // No dars config
            None, // No auth registry for kick
        )
        .await;

        guard.resume().await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await. /kick/status acquires a read lock to serve
        // every poll; if a writer holds the lock during the DB write, every
        // concurrent read blocks for that duration on a slow runner.
        match result {
            Ok(_) => {
                {
                    let mut status = kick_state_clone.status.write().await;
                    *status = KickStatus::Completed;
                }
                tracing::info!("Kick workflow completed successfully");
                mark_run_completed(&db, &instance_for_task).await;
            }
            Err(e) => {
                let msg = format!("{e}");
                {
                    let mut status = kick_state_clone.status.write().await;
                    let mut error = kick_state_clone.error.write().await;
                    *status = KickStatus::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Kick workflow failed: {e}");
                mark_run_failed(&db, &instance_for_task, &msg).await;
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = KickStatus::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(KickResponse {
        status: KickStatus::InProgress,
        message: "Kick workflow started".to_string(),
    })
}

/// Get the current status of the kick workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Kick workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/kick/status")]
pub async fn get_kick_status(kick_state: web::Data<Arc<KickWorkflowState>>) -> impl Responder {
    let status = kick_state.status.read().await;
    let error = kick_state.error.read().await;

    HttpResponse::Ok().json(WorkflowStatusResponse {
        status: *status,
        error: error.clone(),
    })
}

/// Send kick invites to all peers using Noise protocol (excluding the peer being kicked)
async fn send_kick_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    kicked_participant: &CantonId,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invite_message = Message::new_empty(MessageType::InviteKick);

    tracing::info!(
        "Kick invites: self={}, kicked={}",
        current_participant_id,
        kicked_participant
    );

    for peer in &network_config.peers {
        tracing::debug!(
            "Checking peer {}: self_match={}, kicked_match={}",
            peer.participant_id,
            peer.participant_id == *current_participant_id,
            peer.participant_id == *kicked_participant
        );

        // Skip self
        if peer.participant_id == *current_participant_id {
            tracing::debug!("Skipping {} - this is self", peer.participant_id);
            continue;
        }

        // Skip the peer being kicked (they won't participate in the kick workflow)
        if peer.participant_id == *kicked_participant {
            tracing::info!(
                "Skipping invite to {} - they are being kicked",
                peer.participant_id
            );
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!(
                "Skipping invite to {} - no public key configured",
                peer.participant_id
            );
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping invite to {} - invalid public key: {e}",
                    peer.participant_id
                );
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        // Use participant_id as identity (must match what server expects in peer_keys lookup)
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending kick invite to {} at {}:{}",
            peer.participant_id,
            peer.address,
            peer.port
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
                if let Ok(msg) = Message::from_bytes(&response) {
                    if msg.msg_type == MessageType::Ack {
                        tracing::info!("Peer {} acknowledged kick invite", peer.participant_id);
                    } else {
                        tracing::warn!(
                            "Peer {} responded with {msg_type:?} instead of Ack",
                            peer.participant_id,
                            msg_type = msg.msg_type
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to send kick invite to {}: {e}", peer.participant_id);
            }
        }
    }

    Ok(())
}

// ============================================================================
// Onboarding Workflow
// ============================================================================

/// Start an onboarding workflow to create a new decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = OnboardingRequest,
    responses(
        (status = 202, description = "Onboarding workflow started", body = WorkflowResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse),
        (status = 422, description = "Selected peers are not mutually meshed", body = OnboardingMeshErrorResponse)
    )
)]
#[post("/onboarding")]
pub async fn start_onboarding(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    onboarding_state: web::Data<Arc<OnboardingWorkflowState>>,
    body: web::Json<OnboardingRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    // Check if an onboarding is already in progress
    {
        let status = onboarding_state.status.read().await;
        if *status == OnboardingStatus::InProgress {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "An onboarding workflow is already in progress".to_string(),
            });
        }
    }

    // Pre-flight: every selected peer must have every other selected peer in
    // its network config, otherwise the coordinator workflow will hang waiting
    // for peer connections that can never be established.
    match verify_peer_mesh(&data.config, &data.db, &body.peer_ids).await {
        Ok(missing) if !missing.is_empty() => {
            // Tag each edge with its failure mode in the human-readable
            // summary; the structured `missing_edges` array carries the same
            // info via `kind` for the frontend to render targeted hints.
            let edge_summary = missing
                .iter()
                .map(|e| match e.kind {
                    MissingEdgeKind::UnreachableFromCoordinator => format!(
                        "[unreachable from coordinator] {from} → {to}",
                        from = e.from,
                        to = e.to
                    ),
                    MissingEdgeKind::MeshHole => {
                        format!("[mesh hole] {from} → {to}", from = e.from, to = e.to)
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            tracing::warn!(
                "Onboarding rejected: {n} missing peer mesh edge(s): {edge_summary}",
                n = missing.len()
            );
            return HttpResponse::UnprocessableEntity().json(OnboardingMeshErrorResponse {
                error: format!(
                    "Could not verify a full peer mesh. Two failure modes are folded together: \
                     `unreachable from coordinator` (the coordinator cannot query that peer — \
                     it's missing from network config, has no/invalid public key, didn't \
                     answer, or replied with an unparsable peer list — fix the coordinator's \
                     view of `to`, or `to` itself), and `mesh hole` (peer `from` is reachable \
                     but does not have peer `to` in its peer list — add `to` to `from`'s \
                     network config). Edges: {edge_summary}"
                ),
                missing_edges: missing,
            });
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!("Failed to run mesh pre-flight: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to verify peer mesh".into(),
            });
        }
    }

    // Build instance + config up-front so the persisted workflow_runs row
    // can carry the same identifier as the spawned coordinator task.
    let party_id_prefix = body.party_id_prefix.clone();
    let peer_ids = body.peer_ids.clone();
    let instance_name = format!("{party_id_prefix}-creation");
    let onboarding_config =
        workflow::OnboardingConfig::new(party_id_prefix.clone(), instance_name.clone());

    if let Err(e) = insert_coordinator_run(
        &data.db,
        &instance_name,
        WorkflowKind::Onboarding,
        OnboardingStep::WaitingForPeers,
        &onboarding_config,
        &peer_ids,
        None,
    )
    .await
    {
        tracing::warn!("Failed to persist onboarding workflow run: {e:#}");
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to start onboarding workflow: {e}"),
        });
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let onboarding_state_clone = onboarding_state.get_ref().clone();
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();
    *onboarding_state.invited_peers.write().await = peer_ids.clone();
    let party_credentials = data.party_credentials.clone();
    let auth_lock = data.auth.clone();
    let instance_for_task = instance_name.clone();

    // See start_dars below for the rationale: abort_handle, status, and error
    // are flipped under simultaneously-held locks so a concurrent
    // /onboarding/cancel can never observe "status=InProgress + abort_handle=None"
    // and bail.
    let mut abort_guard = onboarding_state.abort_handle.lock().await;
    let mut status_guard = onboarding_state.status.write().await;
    let mut error_guard = onboarding_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send invites to selected peers before starting coordinator workflow
        let invite_result =
            send_onboarding_invites(&config, &db, &peer_ids, &party_id_prefix).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send onboarding invites: {e}");
            guard.resume().await;
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = onboarding_state_clone.status.write().await;
                let mut error = onboarding_state_clone.error.write().await;
                *status = OnboardingStatus::Failed;
                *error = Some(msg.clone());
            }
            mark_run_failed(&db, &instance_for_task, &msg).await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config.clone(),
            db.clone(),
            workflow::WorkflowType::Onboarding,
            Some(onboarding_config),
            None, // No kick config
            None, // No contracts config
            None, // No dars config
            None, // No auth registry for onboarding
        )
        .await;

        guard.resume().await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await. /onboarding/status acquires a read lock to
        // serve every poll; if a writer holds the lock during the DB write,
        // every concurrent read blocks for that duration on a slow runner.
        match result {
            Ok(_) => {
                {
                    let mut status = onboarding_state_clone.status.write().await;
                    *status = OnboardingStatus::Completed;
                }
                tracing::info!("Onboarding workflow completed successfully");
                mark_run_completed(&db, &instance_for_task).await;
                // Operator configures party credentials via the Party Configuration
                // dialog; auto-saving placeholders pollutes the auth registry.

                // Refresh dec_party cache in background
                let bg_config = config.clone();
                let bg_db = db.clone();
                let bg_auth = auth_lock.clone();
                let bg_creds = party_credentials.clone();
                tokio::spawn(async move {
                    let auth = bg_auth.read().await.clone();
                    let creds = bg_creds.read().await.clone();
                    match fetch_decentralized_parties(&bg_config, None, auth, &creds).await {
                        Ok(resp) => {
                            if let Err(e) = store_parties_to_db(&bg_db, "", &resp.parties).await {
                                tracing::warn!("Failed to cache parties after onboarding: {e}");
                                return;
                            }
                            resolve_owner_keys_from_peers(&bg_config, &bg_db, &resp.parties).await;
                            // Audit: report any participants whose owner_key
                            // is still NULL after resolve. Not fatal — Noise
                            // resolution may run again on the next stale
                            // refresh — but unexpected for a freshly
                            // onboarded party.
                            for party in &resp.parties {
                                for p in &party.participants {
                                    let uid = p.participant_uid.to_string();
                                    match bg_db
                                        .get_dec_party_participant_owner_key(&party.party_id, &uid)
                                        .await
                                    {
                                        Ok(Some(_)) => {} // resolved
                                        Ok(None) => tracing::warn!(
                                            party_id = %party.party_id,
                                            participant_uid = %uid,
                                            "Participant owner_key unresolved after onboarding"
                                        ),
                                        Err(e) => tracing::warn!(
                                            party_id = %party.party_id,
                                            participant_uid = %uid,
                                            error = %e,
                                            "Failed to read owner_key from cache during post-onboarding audit"
                                        ),
                                    }
                                }
                            }
                        }
                        Err(e) => tracing::warn!("Failed to refresh parties after onboarding: {e}"),
                    }
                });
            }
            Err(e) => {
                let msg = format!("{e}");
                {
                    let mut status = onboarding_state_clone.status.write().await;
                    let mut error = onboarding_state_clone.error.write().await;
                    *status = OnboardingStatus::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Onboarding workflow failed: {e}");
                mark_run_failed(&db, &instance_for_task, &msg).await;
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = OnboardingStatus::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(OnboardingResponse {
        status: OnboardingStatus::InProgress,
        message: "Onboarding workflow started".to_string(),
    })
}

/// Get the current status of the onboarding workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Onboarding workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/onboarding/status")]
pub async fn get_onboarding_status(
    onboarding_state: web::Data<Arc<OnboardingWorkflowState>>,
) -> impl Responder {
    let status = onboarding_state.status.read().await;
    let error = onboarding_state.error.read().await;

    HttpResponse::Ok().json(WorkflowStatusResponse {
        status: *status,
        error: error.clone(),
    })
}

/// Send onboarding invites to selected peers using Noise protocol
async fn send_onboarding_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    party_id_prefix: &str,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let payload = OnboardingInvitePayload {
        prefix: party_id_prefix.to_string(),
        participants: peer_ids.to_vec(),
    };
    let payload_bytes = serde_json::to_vec(&payload)?;
    let invite_message = Message::new(MessageType::InviteOnboarding, payload_bytes);

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
        // Use participant_id as identity (must match what server expects in peer_keys lookup)
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending onboarding invite to {peer_id} at {addr}:{port}",
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
                if let Ok(msg) = Message::from_bytes(&response) {
                    if msg.msg_type == MessageType::Ack {
                        tracing::info!("Peer {peer_id} acknowledged invite");
                    } else {
                        tracing::warn!(
                            "Peer {peer_id} responded with {msg_type:?} instead of Ack",
                            msg_type = msg.msg_type
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to send invite to {peer_id}: {e}");
            }
        }
    }

    Ok(())
}

/// Pre-flight check: query each selected peer for its known peer list and
/// verify every pair within `peer_ids` knows each other. Returns the list of
/// missing directed edges (`from` does not have `to` configured). Empty Vec
/// on success.
///
/// Coordinator → peer reachability is implicitly verified: if a peer can
/// answer our Noise call, it already has us in its peer list. We only check
/// peer ↔ peer mesh — that's the case the user can't see otherwise.
async fn verify_peer_mesh(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
) -> Result<Vec<MissingPeerEdge>> {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let request = Message::new_empty(MessageType::ListPeers);
    let identity = config.participant_id().to_string();

    let mut missing_edges = Vec::new();
    let mut peer_views: HashMap<String, HashSet<String>> = HashMap::new();

    for peer_id in peer_ids {
        let peer = match network_config
            .peers
            .iter()
            .find(|p| &p.participant_id == peer_id)
        {
            Some(p) => p,
            None => {
                // Coordinator doesn't know this peer — won't be able to invite them.
                missing_edges.push(MissingPeerEdge {
                    from: identity.clone(),
                    to: peer_id.to_string(),
                    kind: MissingEdgeKind::UnreachableFromCoordinator,
                });
                continue;
            }
        };

        if peer.public_key.is_empty() {
            tracing::warn!("Peer {peer_id} has no public key configured — cannot mesh-check");
            missing_edges.push(MissingPeerEdge {
                from: identity.clone(),
                to: peer_id.to_string(),
                kind: MissingEdgeKind::UnreachableFromCoordinator,
            });
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!("Peer {peer_id} has invalid public key: {e}");
                missing_edges.push(MissingPeerEdge {
                    from: identity.clone(),
                    to: peer_id.to_string(),
                    kind: MissingEdgeKind::UnreachableFromCoordinator,
                });
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);

        let response = match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &request,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Failed to query peers from {peer_id}: {e}");
                missing_edges.push(MissingPeerEdge {
                    from: identity.clone(),
                    to: peer_id.to_string(),
                    kind: MissingEdgeKind::UnreachableFromCoordinator,
                });
                continue;
            }
        };

        let msg = match Message::from_bytes(&response) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Malformed response from {peer_id}: {e}");
                missing_edges.push(MissingPeerEdge {
                    from: identity.clone(),
                    to: peer_id.to_string(),
                    kind: MissingEdgeKind::UnreachableFromCoordinator,
                });
                continue;
            }
        };

        if msg.msg_type != MessageType::PeerList {
            tracing::warn!(
                "Peer {peer_id} responded with {msg_type:?} instead of PeerList",
                msg_type = msg.msg_type
            );
            missing_edges.push(MissingPeerEdge {
                from: identity.clone(),
                to: peer_id.to_string(),
                kind: MissingEdgeKind::UnreachableFromCoordinator,
            });
            continue;
        }

        let view: HashSet<String> = match serde_json::from_slice(&msg.payload) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Could not parse peer list from {peer_id}: {e}");
                missing_edges.push(MissingPeerEdge {
                    from: identity.clone(),
                    to: peer_id.to_string(),
                    kind: MissingEdgeKind::UnreachableFromCoordinator,
                });
                continue;
            }
        };
        peer_views.insert(peer_id.to_string(), view);
    }

    // For every directed pair within selected peers, check that A's peer view
    // includes B. Missing means A and B aren't mutually connected.
    for a in peer_ids {
        let Some(a_view) = peer_views.get(&a.to_string()) else {
            // Already recorded a coordinator→A reachability problem above.
            continue;
        };
        for b in peer_ids {
            if a == b {
                continue;
            }
            if !a_view.contains(&b.to_string()) {
                missing_edges.push(MissingPeerEdge {
                    from: a.to_string(),
                    to: b.to_string(),
                    kind: MissingEdgeKind::MeshHole,
                });
            }
        }
    }

    Ok(missing_edges)
}

// ============================================================================
// Contracts Workflow
// ============================================================================

/// Start a contracts workflow to upload DARs and create contracts
#[utoipa::path(
    tag = "Workflows",
    request_body = ContractsRequest,
    responses(
        (status = 202, description = "Contracts workflow started", body = WorkflowResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse)
    )
)]
#[post("/contracts")]
pub async fn start_contracts(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    contracts_state: web::Data<Arc<ContractsWorkflowState>>,
    body: web::Json<ContractsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    // Check if a contracts workflow is already in progress
    {
        let status = contracts_state.status.read().await;
        if *status == WorkflowProgress::InProgress {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "A contracts workflow is already in progress".to_string(),
            });
        }
    }

    // Create contracts config from request
    let timestamp = now_secs();
    let instance_name = format!(
        "{}-contracts-{timestamp}",
        body.decentralized_party_id.prefix
    );
    let contracts_config = workflow::ContractsConfig::new(
        body.decentralized_party_id.clone(),
        body.participant_ids.clone(),
        body.participant_parties.clone(),
        body.operator_party.clone(),
        body.contracts.clone(),
        instance_name,
    );

    // Track invitees for /cancel: every peer except self.
    let contracts_invitees: Vec<CantonId> = match data.db.get_all_peers().await {
        Ok(peers) => peers
            .into_iter()
            .map(|p| p.participant_id)
            .filter(|p| p != data.config.participant_id())
            .collect(),
        Err(e) => {
            tracing::warn!("Failed to load peers for cancel-invite tracking: {e}");
            Vec::new()
        }
    };

    let instance_name_for_run = contracts_config.instance_name.clone();
    if let Err(e) = insert_coordinator_run(
        &data.db,
        &instance_name_for_run,
        WorkflowKind::Contracts,
        ContractsStep::WaitingForPeers,
        &contracts_config,
        &contracts_invitees,
        Some(body.decentralized_party_id.clone()),
    )
    .await
    {
        tracing::warn!("Failed to persist contracts workflow run: {e:#}");
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to start contracts workflow: {e}"),
        });
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let workflow_auth = data.auth.read().await.clone();
    let auth_lock = data.auth.clone();
    let contracts_state_clone = contracts_state.get_ref().clone();
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();
    let party_credentials = data.party_credentials.clone();
    *contracts_state.invited_peers.write().await = contracts_invitees;
    let instance_for_task = instance_name_for_run.clone();

    // See start_dars below for the rationale: abort_handle, status, and error
    // are flipped under simultaneously-held locks so a concurrent
    // /contracts/cancel can never observe "status=InProgress + abort_handle=None"
    // and bail.
    let mut abort_guard = contracts_state.abort_handle.lock().await;
    let mut status_guard = contracts_state.status.write().await;
    let mut error_guard = contracts_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send invites to all peers before starting coordinator workflow
        let invite_result = send_contracts_invites(&config, &db).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send contracts invites: {e}");
            guard.resume().await;
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = contracts_state_clone.status.write().await;
                let mut error = contracts_state_clone.error.write().await;
                *status = WorkflowProgress::Failed;
                *error = Some(msg.clone());
            }
            mark_run_failed(&db, &instance_for_task, &msg).await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config.clone(),
            db.clone(),
            workflow::WorkflowType::Contracts,
            None, // No onboarding config
            None, // No kick config
            Some(contracts_config.clone()),
            None, // No dars config
            workflow_auth,
        )
        .await;

        guard.resume().await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await. /contracts/status acquires a read lock to
        // serve every poll; if a writer holds the lock during the DB write,
        // every concurrent read blocks for that duration on a slow runner.
        match result {
            Ok(_) => {
                {
                    let mut status = contracts_state_clone.status.write().await;
                    *status = WorkflowProgress::Completed;
                }
                tracing::info!("Contracts workflow completed successfully");
                mark_run_completed(&db, &instance_for_task).await;

                // Refresh dec_party cache to pick up new contracts
                let bg_config = config.clone();
                let bg_db = db.clone();
                let bg_auth = auth_lock.clone();
                let bg_creds = party_credentials.clone();
                tokio::spawn(async move {
                    let auth = bg_auth.read().await.clone();
                    let creds = bg_creds.read().await.clone();
                    match fetch_decentralized_parties(&bg_config, None, auth, &creds).await {
                        Ok(resp) => {
                            if let Err(e) = store_parties_to_db(&bg_db, "", &resp.parties).await {
                                tracing::warn!(
                                    "Failed to cache parties after contract deployment: {e}"
                                );
                            } else {
                                resolve_owner_keys_from_peers(&bg_config, &bg_db, &resp.parties)
                                    .await;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to refresh parties after contract deployment: {e}"
                            );
                        }
                    }
                });
            }
            Err(e) => {
                let msg = format!("{e}");
                {
                    let mut status = contracts_state_clone.status.write().await;
                    let mut error = contracts_state_clone.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Contracts workflow failed: {e}");
                mark_run_failed(&db, &instance_for_task, &msg).await;
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
        message: "Contracts workflow started".to_string(),
    })
}

/// Get the current status of the contracts workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Contracts workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/contracts/status")]
pub async fn get_contracts_status(
    contracts_state: web::Data<Arc<ContractsWorkflowState>>,
) -> impl Responder {
    let status = contracts_state.status.read().await;
    let error = contracts_state.error.read().await;

    HttpResponse::Ok().json(WorkflowStatusResponse {
        status: *status,
        error: error.clone(),
    })
}

// ============================================================================
// DARs Upload (Local)
// ============================================================================

/// Upload DAR files to the current node only (no distribution to peers)
#[utoipa::path(
    tag = "Workflows",
    request_body = DarsRequest,
    responses(
        (status = 200, description = "DARs uploaded to local node", body = SuccessResponse),
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
        (status = 409, description = "Workflow already in progress", body = ErrorResponse)
    )
)]
#[post("/dars/distribute")]
pub async fn start_dars(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    dars_state: web::Data<Arc<DarsWorkflowState>>,
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

    // Check if a DARs workflow is already in progress
    {
        let status = dars_state.status.read().await;
        if *status == WorkflowProgress::InProgress {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "A DARs distribution workflow is already in progress".to_string(),
            });
        }
    }

    // Create DARs config from request
    let timestamp = now_secs();
    let instance_name = format!("dars-distribute-{timestamp}");
    let dars_config = workflow::DarsConfig {
        dar_files: body.dar_files.clone(),
        instance_name: instance_name.clone(),
        peer_ids: body.peer_ids.clone(),
    };

    if let Err(e) = insert_coordinator_run(
        &data.db,
        &instance_name,
        WorkflowKind::Dars,
        DarsStep::WaitingForPeers,
        &dars_config,
        &body.peer_ids,
        None,
    )
    .await
    {
        tracing::warn!("Failed to persist dars workflow run: {e:#}");
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to start DARs workflow: {e}"),
        });
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let dars_state_clone = dars_state.get_ref().clone();
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();
    let peer_ids = body.peer_ids.clone();
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
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send invites to selected peers before starting coordinator workflow
        let dar_filenames: Vec<String> = dars_config
            .dar_files
            .iter()
            .map(|f| f.filename.clone())
            .collect();
        let invite_result = send_dars_invites(&config, &db, &peer_ids, &dar_filenames).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send DARs invites: {e}");
            guard.resume().await;
            let mut status = dars_state_clone.status.write().await;
            let mut error = dars_state_clone.error.write().await;
            *status = WorkflowProgress::Failed;
            *error = Some(format!("Failed to send invites: {e}"));
            mark_run_failed(
                &db,
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
            None, // No auth
        )
        .await;

        guard.resume().await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await (see kick/onboarding/contracts handlers above).
        match result {
            Ok(_) => {
                {
                    let mut status = dars_state_clone.status.write().await;
                    *status = WorkflowProgress::Completed;
                }
                tracing::info!("DARs distribution workflow completed successfully");
                mark_run_completed(&db, &instance_for_task).await;
            }
            Err(e) => {
                let msg = format!("{e}");
                {
                    let mut status = dars_state_clone.status.write().await;
                    let mut error = dars_state_clone.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("DARs distribution workflow failed: {e}");
                mark_run_failed(&db, &instance_for_task, &msg).await;
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
pub async fn get_dars_status(dars_state: web::Data<Arc<DarsWorkflowState>>) -> impl Responder {
    let status = dars_state.status.read().await;
    let error = dars_state.error.read().await;

    HttpResponse::Ok().json(WorkflowStatusResponse {
        status: *status,
        error: error.clone(),
    })
}

/// Send DARs invites to selected peers using Noise protocol
async fn send_dars_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    dar_filenames: &[String],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let payload = DarsInvitePayload {
        dar_filenames: dar_filenames.to_vec(),
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
                if let Ok(msg) = Message::from_bytes(&response) {
                    if msg.msg_type == MessageType::Ack {
                        tracing::info!("Peer {peer_id} acknowledged DARs invite");
                    } else {
                        tracing::warn!(
                            "Peer {peer_id} responded with {msg_type:?} instead of Ack",
                            msg_type = msg.msg_type
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to send DARs invite to {peer_id}: {e}");
            }
        }
    }

    Ok(())
}

/// Shared cancel logic. All four workflow types use HttpWorkflowState<WorkflowProgress>.
async fn cancel_workflow_state(
    state: &Arc<HttpWorkflowState<WorkflowProgress>>,
    data: &web::Data<AppState>,
    label: &str,
    kind: WorkflowKind,
) -> HttpResponse {
    {
        let status = state.status.read().await;
        if *status != WorkflowProgress::InProgress {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!("No {label} workflow in progress"),
            });
        }
    }

    // Take the abort handle FIRST. If it's None we're racing with a start path that
    // hasn't finished setting itself up yet; refuse the cancel rather than mark the
    // workflow Cancelled while the spawned task is still alive. Start paths always
    // populate abort_handle before they let any await reach this point.
    let Some(handle) = state.abort_handle.lock().await.take() else {
        tracing::warn!(
            "{label} cancel arrived before the workflow finished initializing — refusing"
        );
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("{label} workflow is still initializing — try again in a moment"),
        });
    };
    handle.abort();

    {
        let mut control = data.noise_listener_control.write().await;
        control.should_pause = false;
    }
    data.noise_listener_notify.notify_one();

    let invitees = state.invited_peers.read().await.clone();
    if !invitees.is_empty()
        && let Err(e) = send_cancel_invites(&data.config, &data.db, &invitees).await
    {
        tracing::warn!("send_cancel_invites failed during {label} cancel: {e}");
    }

    {
        let mut status = state.status.write().await;
        *status = WorkflowProgress::Cancelled;
    }
    {
        let mut error = state.error.write().await;
        *error = None;
    }

    // Mirror the cancel into the persisted workflow_runs row so the feed picks it up.
    if let Ok(Some(run)) = data
        .db
        .get_active_workflow_run(kind, WorkflowRole::Coordinator)
        .await
        && let Err(e) = mark_run_status(
            &data.db,
            &run.instance_name,
            WorkflowProgress::Cancelled,
            None,
        )
        .await
    {
        tracing::warn!(
            "Failed to flip workflow_runs row {} to cancelled: {e:#}",
            run.instance_name
        );
    }

    tracing::info!("{label} workflow cancelled");
    HttpResponse::Ok().json(MessageResponse {
        message: format!("{label} workflow cancelled"),
    })
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/onboarding/cancel")]
pub async fn cancel_onboarding(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    state: web::Data<Arc<OnboardingWorkflowState>>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(
        state.get_ref(),
        &data,
        "Onboarding",
        WorkflowKind::Onboarding,
    )
    .await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/kick/cancel")]
pub async fn cancel_kick(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    state: web::Data<Arc<KickWorkflowState>>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(state.get_ref(), &data, "Kick", WorkflowKind::Kick).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/contracts/cancel")]
pub async fn cancel_contracts(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    state: web::Data<Arc<ContractsWorkflowState>>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(state.get_ref(), &data, "Contracts", WorkflowKind::Contracts).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/dars/cancel")]
pub async fn cancel_dars(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    state: web::Data<Arc<DarsWorkflowState>>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(state.get_ref(), &data, "DARs", WorkflowKind::Dars).await
}

// ============================================================================
// Generic workflow_runs endpoints (used by the unified notifications feed)
// ============================================================================

/// List every workflow run that should appear in the notifications feed:
/// every InProgress run on this node + any terminal run the operator hasn't
/// dismissed yet. `coordinator_name` is joined from the peers table.
#[utoipa::path(
    tag = "Workflows",
    responses((status = 200, description = "Visible workflow runs", body = WorkflowRunsResponse))
)]
#[get("/workflows")]
pub async fn list_workflows(data: web::Data<AppState>) -> impl Responder {
    let runs = match data.db.get_visible_workflow_runs().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to list workflow runs: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to list workflow runs: {e}"),
            });
        }
    };

    // Resolve coordinator names from the peers table — same pattern get_invitations uses.
    let pubkey_to_name: HashMap<String, String> = data
        .db
        .get_all_peers()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.public_key, p.name))
        .collect();

    let resolved: Vec<WorkflowRun> = runs
        .into_iter()
        .map(|mut r| {
            if let Some(pk) = r.coordinator_pubkey.as_deref() {
                r.coordinator_name = pubkey_to_name.get(pk).cloned();
            }
            r
        })
        .collect();

    HttpResponse::Ok().json(WorkflowRunsResponse { runs: resolved })
}

/// Mark a terminal-state workflow run as dismissed so it disappears from the
/// notifications feed. Returns 409 if the run is still InProgress.
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Run dismissed", body = MessageResponse),
        (status = 404, description = "Run not found", body = ErrorResponse),
        (status = 409, description = "Run is still in progress", body = ErrorResponse)
    )
)]
#[post("/workflows/{instance_name}/dismiss")]
pub async fn dismiss_workflow(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let instance_name = path.into_inner();

    let run = match data.db.get_workflow_run(&instance_name).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: format!("workflow run {instance_name} not found"),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to load workflow run: {e}"),
            });
        }
    };

    if run.status == WorkflowProgress::InProgress {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "{} workflow {instance_name} is still in progress — cancel it first",
                run.kind
            ),
        });
    }

    let mut tx = match data.db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to begin tx: {e}"),
            });
        }
    };
    if let Err(e) = tx.dismiss_workflow_run(&instance_name).await {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to dismiss workflow run: {e}"),
        });
    }
    if let Err(e) = Commitable::commit(tx).await {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to commit dismiss: {e}"),
        });
    }

    HttpResponse::Ok().json(MessageResponse {
        message: format!("workflow {instance_name} dismissed"),
    })
}

/// Retry a Failed coordinator-side workflow run from where it left off.
///
/// Flips `status` back to `inprogress`, clears `error`, and re-spawns the
/// coordinator task. `NoiseServer::new` re-hydrates `WorkflowState` from the
/// persisted `current_step`, so the run picks up at the same step that
/// failed. Only valid on Failed coordinator-side rows; peer retry is not
/// supported (the coordinator may already be past the config-bearing
/// command — operator should dismiss the peer row and re-accept the
/// invite instead).
#[utoipa::path(
    tag = "Workflows",
    params(
        ("instance_name" = String, Path, description = "Workflow run identifier")
    ),
    responses(
        (status = 200, description = "Retry started", body = MessageResponse),
        (status = 404, description = "Run not found", body = ErrorResponse),
        (status = 409, description = "Run is not in a retryable state", body = ErrorResponse)
    )
)]
#[post("/workflows/{instance_name}/retry")]
pub async fn retry_workflow(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    kick_state: web::Data<Arc<KickWorkflowState>>,
    onboarding_state: web::Data<Arc<OnboardingWorkflowState>>,
    contracts_state: web::Data<Arc<ContractsWorkflowState>>,
    dars_state: web::Data<Arc<DarsWorkflowState>>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let instance_name = path.into_inner();

    let run = match data.db.get_workflow_run(&instance_name).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: format!("workflow run {instance_name} not found"),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to load workflow run: {e}"),
            });
        }
    };

    if run.role != WorkflowRole::Coordinator {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: "Retry must be initiated from the coordinator side. Peer rows flip back \
                    to InProgress automatically when the coordinator retries — wait for that or \
                    dismiss the row."
                .to_string(),
        });
    }
    if run.status != WorkflowProgress::Failed {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Cannot retry a workflow in status {:?}; only Failed runs can be retried",
                run.status
            ),
        });
    }

    // Flip the row back to InProgress so the partial unique index reserves
    // (kind, role) again before we spawn the resumed task. If a fresh run of
    // the same kind+role has been started since this one failed, that index
    // will reject this update and we surface a 409.
    let mut tx = match data.db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to begin tx: {e}"),
            });
        }
    };
    if let Err(e) = tx
        .set_workflow_run_status(
            &instance_name,
            WorkflowProgress::InProgress,
            None,
            now_secs(),
        )
        .await
    {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to flip status to InProgress: {e}"),
        });
    }
    if let Err(e) = Commitable::commit(tx).await {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to commit retry status: {e}"),
        });
    }

    // Re-load the row (now InProgress), broadcast RetryWorkflow to the
    // peer cohort, and re-spawn the coordinator task. respawn_coordinator
    // updates the matching HttpWorkflowState, stashes a new abort handle,
    // and finalizes the row to Completed/Failed when the task ends.
    let run = match data.db.get_workflow_run(&instance_name).await {
        Ok(Some(r)) => r,
        _ => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Workflow run vanished mid-retry".to_string(),
            });
        }
    };
    // Tell every previously-invited peer to flip their Failed row back to
    // InProgress and re-spin start_peer. Best-effort — peers that are
    // unreachable now will stay Failed; operator can dismiss + re-accept.
    if let Err(e) = send_retry_workflow(&data.config, &data.db, &run.expected_peers).await {
        tracing::warn!("Failed to broadcast RetryWorkflow: {e:#}");
    }
    respawn_coordinator(
        data.db.clone(),
        data.config.clone(),
        &run,
        kick_state,
        onboarding_state,
        contracts_state,
        dars_state,
        data.noise_listener_control.clone(),
        data.noise_listener_notify.clone(),
        data.auth.clone(),
    )
    .await;

    HttpResponse::Ok().json(MessageResponse {
        message: format!(
            "Retrying workflow {instance_name} from step {}",
            run.current_step
        ),
    })
}

/// Best-effort: notify previously-invited peers that the workflow is cancelled
/// so they can drop the matching pending invitation.
async fn send_cancel_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
) -> Result {
    broadcast_simple_message(
        config,
        db,
        peer_ids,
        Message::new_empty(MessageType::CancelInvite),
        "CancelInvite",
    )
    .await
}

/// Best-effort: notify previously-invited peers that the coordinator is
/// retrying the workflow so they flip their Failed row back to InProgress
/// and re-spin `start_peer`.
async fn send_retry_workflow(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
) -> Result {
    broadcast_simple_message(
        config,
        db,
        peer_ids,
        Message::new_empty(MessageType::RetryWorkflow),
        "RetryWorkflow",
    )
    .await
}

async fn broadcast_simple_message(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    message: Message,
    label: &str,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let identity = config.participant_id().to_string();

    for peer_id in peer_ids {
        let Some(peer) = network_config
            .peers
            .iter()
            .find(|p| &p.participant_id == peer_id)
        else {
            continue;
        };
        if peer.public_key.is_empty() {
            continue;
        }
        let Ok(peer_pub_key) = parse_public_key(&peer.public_key) else {
            continue;
        };
        let psk = keypair.derive_psk(&peer_pub_key);

        if let Err(e) = send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &message,
        )
        .await
        {
            tracing::warn!("Failed to send {label} to {peer_id}: {e}");
        }
    }
    Ok(())
}

/// Send contracts invites to all peers using Noise protocol
async fn send_contracts_invites(config: &NodeConfig, db: &SqlitePool) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invite_message = Message::new_empty(MessageType::InviteContracts);

    for peer in &network_config.peers {
        if peer.participant_id == *current_participant_id {
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!(
                "Skipping invite to {} - no public key configured",
                peer.participant_id
            );
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping invite to {} - invalid public key: {e}",
                    peer.participant_id
                );
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        // Use participant_id as identity (must match what server expects in peer_keys lookup)
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending contracts invite to {} at {}:{}",
            peer.participant_id,
            peer.address,
            peer.port
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
                if let Ok(msg) = Message::from_bytes(&response) {
                    if msg.msg_type == MessageType::Ack {
                        tracing::info!(
                            "Peer {} acknowledged contracts invite",
                            peer.participant_id
                        );
                    } else {
                        tracing::warn!(
                            "Peer {} responded with {msg_type:?} instead of Ack",
                            peer.participant_id,
                            msg_type = msg.msg_type
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    "Failed to send contracts invite to {}: {e}",
                    peer.participant_id
                );
            }
        }
    }

    Ok(())
}
