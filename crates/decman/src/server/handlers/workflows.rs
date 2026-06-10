use std::{
    collections::{HashMap, HashSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use anyhow::Context;
use sqlx::SqlitePool;

use super::parties::{
    fetch_decentralized_parties, resolve_owner_keys_from_peers, store_parties_to_db,
};
use crate::{
    canton_id::{CantonId, validate_party_id_prefix},
    config::{NetworkConfig, NodeConfig},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    server::{
        AppState,
        middleware::require_admin,
        respawn_coordinator,
        types::{
            ContractsInvitePayload, ContractsRequest, DarsInvitePayload, DarsRequest,
            ErrorResponse, KickInvitePayload, KickRequest, KickResponse, KickStatus,
            MessageResponse, MissingEdgeKind, MissingPeerEdge, OnboardingInvitePayload,
            OnboardingMeshErrorResponse, OnboardingRequest, OnboardingResponse, OnboardingStatus,
            SuccessResponse, WorkflowGuard, WorkflowInstance, WorkflowKind, WorkflowProgress,
            WorkflowResponse, WorkflowRole, WorkflowRun, WorkflowRunsResponse,
            WorkflowStatusResponse,
        },
    },
    workflow::{self, ContractsStep, DarsStep, KickStep, OnboardingStep, state::WorkflowStep},
};

// ============================================================================
// Workflow State Types
// ============================================================================

// Per-kind workflow state is now tracked per-instance in
// `AppState.workflows` (`WorkflowRegistry`) rather than via singleton
// `HttpWorkflowState<S>` aliases.

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
        coordinator_instance: None,
        coordinator_name: None,
        expected_peers: invitees.to_vec(),
        completed_peers: Vec::new(),
        dec_party_id,
        prefix: None,
        participants: Vec::new(),
        previous_threshold: None,
        new_threshold: None,
        kicked_participant: None,
        package_names: Vec::new(),
        dar_filenames: Vec::new(),
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

    // Scope the kick to this dec party's actual members, not every peer this
    // node has configured. A party can be a strict subset of the configured
    // mesh (see start_onboarding), and basing the signer set / threshold bound
    // / invites on the full mesh would invite outsiders and stall
    // WaitingForPeers on peers that were never part of the party. Fall back to
    // the full peer set only if no cached row yields a usable id — membership
    // not cached yet, or every `participant_uid` in a legacy/invalid format —
    // since scoping to an empty member set would reject the kick with a
    // misleading "need at least 2 party members" error.
    let party_member_ids: HashSet<CantonId> = match data
        .db
        .get_dec_party_participants(&decentralized_party_id)
        .await
    {
        Ok(rows) => {
            let parsed: HashSet<CantonId> = rows
                .iter()
                .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
                .collect();
            if parsed.is_empty() {
                tracing::warn!(
                    "No usable cached participants for {decentralized_party_id} \
                     ({count} rows); falling back to all configured peers for \
                     kick scoping",
                    count = rows.len()
                );
                peers.iter().map(|p| p.participant_id.clone()).collect()
            } else {
                parsed
            }
        }
        Err(e) => {
            tracing::error!("Failed to load dec party participants for kick: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load decentralized party members".to_string(),
            });
        }
    };
    let peers: Vec<_> = peers
        .into_iter()
        .filter(|p| party_member_ids.contains(&p.participant_id))
        .collect();

    // Preconditions for a kick: there must be at least one peer left
    // after the kick (so the surviving signer set is non-empty), and the
    // participant being kicked must be a known peer. Without these checks
    // the `post_kick_member_count` below could go negative and produce
    // a "between 1 and -1" error that obscures the real problem.
    if peers.len() < 2 {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "Cannot kick: need at least 2 party members (this node + the target), \
                 have {n}",
                n = peers.len(),
            ),
        });
    }
    if !peers.iter().any(|p| p.participant_id == participant_id) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "Cannot kick {participant_id}: not a member of this decentralized party"
            ),
        });
    }

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
                 (party member count {n}, minus the participant being kicked); got {got}",
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

    // Register this run in the shared registry. A duplicate `instance_name`
    // (extremely unlikely given the timestamp) is the only rejection — multiple
    // distinct kick runs may now proceed concurrently.
    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::Kick,
        WorkflowRole::Coordinator,
    );
    let kick_state = &instance.http;

    let kick_config = workflow::KickConfig::new(
        decentralized_party_id.clone(),
        participant_id.clone(),
        namespace_fingerprint,
        body.new_threshold,
        body.previous_threshold,
        instance_name.clone(),
    );

    // Register in the registry FIRST — atomic dedup. A racing request for the
    // same instance_name must be rejected here, BEFORE we persist, so it can't
    // upsert (and clobber) the in-flight run's workflow_runs row.
    if !data.workflows.insert(instance.clone()) {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "A workflow with instance {} is already running",
                instance.instance_name
            ),
        });
    }

    // Persist the workflow_runs row. On failure, unregister so we don't leak a
    // stale registry entry for a run that never started.
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
        data.workflows.remove(&instance_name);
        tracing::warn!("Failed to persist kick workflow run: {e:#}");
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to start kick workflow: {e}"),
        });
    }

    let invitees_for_invites = invitees.clone();
    *kick_state.invited_peers.write().await = invitees;

    // Spawn the kick workflow in the background
    let config = data.config.clone();
    let db = data.db.clone();
    let kick_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let workflows = data.workflows.clone();
    let last_seen = data.last_seen.clone();
    let instance_for_task = instance_name.clone();

    // See start_dars below for the rationale: abort_handle, status, and error
    // are flipped under simultaneously-held locks so a concurrent /kick/cancel
    // can never observe "status=InProgress + abort_handle=None" and bail.
    let mut abort_guard = kick_state.abort_handle.lock().await;
    let mut status_guard = kick_state.status.write().await;
    let mut error_guard = kick_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        // Removes this run from the registry on return (success/failure/abort).
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send kick invites to the surviving party members before starting workflow
        let invite_result =
            send_kick_invites(&config, &db, &kick_config, &invitees_for_invites).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send kick invites: {e}");
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
            last_seen,
            instance_for_coord,
        )
        .await;

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
pub async fn get_kick_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Kick).await)
}

/// Summarize the status of a coordinator run of `kind` for the legacy
/// per-kind `/{kind}/status` endpoints.
///
/// While a run is live it is in the registry, so report its in-memory status.
/// But the registry entry is removed the moment a run reaches a terminal
/// status (its `WorkflowGuard` drops), so once finished there is nothing in the
/// registry — fall back to the **latest persisted `workflow_runs` row** of this
/// kind, which carries the terminal Completed/Failed/Cancelled status (and is
/// the same source the `/workflows` feed uses). Without this fallback a poller
/// watching `/{kind}/status` would never observe completion. With concurrent
/// runs of one kind these endpoints are necessarily coarse — callers wanting
/// per-instance detail should use `GET /workflows`.
async fn kind_status(data: &web::Data<AppState>, kind: WorkflowKind) -> WorkflowStatusResponse {
    // Pick deterministically (lowest instance_name) so repeated polls don't flip
    // between concurrent same-kind runs — `snapshot()` is HashMap-ordered.
    let mut live: Vec<_> = data
        .workflows
        .snapshot()
        .into_iter()
        .filter(|i| i.kind == kind && i.role == WorkflowRole::Coordinator)
        .collect();
    live.sort_by(|a, b| a.instance_name.cmp(&b.instance_name));
    if let Some(inst) = live.first() {
        return WorkflowStatusResponse {
            status: *inst.http.status.read().await,
            error: inst.http.error.read().await.clone(),
        };
    }
    // No live run registered — report the latest persisted coordinator run of
    // this kind (terminal status survives in the DB after deregistration).
    if let Ok(runs) = SchemaRead::get_visible_workflow_runs(&data.db).await
        && let Some(run) = runs
            .into_iter()
            .filter(|r| r.kind == kind && r.role == WorkflowRole::Coordinator)
            .max_by_key(|r| r.created_at)
    {
        return WorkflowStatusResponse {
            status: run.status,
            error: run.error,
        };
    }
    WorkflowStatusResponse {
        status: WorkflowProgress::default(),
        error: None,
    }
}

/// Send kick invites to all peers using Noise protocol (excluding the peer being kicked)
async fn send_kick_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    kick_config: &workflow::KickConfig,
    invitees: &[CantonId],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let kicked_participant = &kick_config.participant_id;
    let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
    let payload = KickInvitePayload {
        dec_party_id: kick_config.decentralized_party_id.clone(),
        kicked_participant: kicked_participant.clone(),
        new_threshold: kick_config.new_threshold,
        previous_threshold: kick_config.previous_threshold,
        participants: invitees.to_vec(),
        workflow_instance: Some(kick_config.instance_name.clone()),
    };
    let payload_bytes = serde_json::to_vec(&payload).context("encode KickInvitePayload")?;
    let invite_message = Message::new(MessageType::InviteKick, payload_bytes);

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

        // Only invite this run's surviving party members (see start_kick). The
        // self/kicked skips below are now subsumed by this but kept for clarity.
        if !invitee_set.contains(&peer.participant_id) {
            continue;
        }

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
                interpret_invite_reply(&peer.participant_id, "kick", &response)?;
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
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse),
        (status = 422, description = "Selected peers are not mutually meshed", body = OnboardingMeshErrorResponse)
    )
)]
#[post("/onboarding")]
pub async fn start_onboarding(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<OnboardingRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    // Reject an invalid party prefix up-front: it becomes the identifier part
    // of the Canton party id (`<prefix>::<namespace>`), and a bad character
    // would otherwise fail deep in the workflow as an opaque Canton proto
    // deserialization error ~90s later. Fail fast with a clear 400 instead.
    if let Err(msg) = validate_party_id_prefix(&body.party_id_prefix) {
        return HttpResponse::BadRequest().json(ErrorResponse { error: msg });
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
    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::Onboarding,
        WorkflowRole::Coordinator,
    );
    let onboarding_state = &instance.http;
    let onboarding_config =
        workflow::OnboardingConfig::new(party_id_prefix.clone(), instance_name.clone());

    // Refuse onboarding when a party with this prefix already exists. The
    // human-readable prefix is the only piece of the party id the operator
    // chooses; allowing duplicates makes the parties list ambiguous and —
    // when the participant set also matches — the workflow silently
    // converges onto the existing party (Canton's DNS hash is deterministic
    // from owners + threshold). Surface a clear 409 upfront instead.
    match find_party_with_prefix(&data.db, &party_id_prefix).await {
        Ok(Some(existing_party_id)) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!(
                    "A decentralized party with the prefix '{party_id_prefix}' already exists \
                     ({existing_party_id}). Choose a different prefix."
                ),
            });
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("Failed to check for duplicate-prefix party: {e:#}");
        }
    }

    // Register FIRST — atomic dedup before persist. Onboarding's instance_name
    // is deterministic (`{prefix}-creation`), so a racing same-prefix request
    // is realistic; rejecting here (before the upsert) stops it clobbering the
    // in-flight run's persisted row.
    if !data.workflows.insert(instance.clone()) {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "A workflow with instance {} is already running",
                instance.instance_name
            ),
        });
    }

    // Persist; unregister on failure so a never-started run can't leak.
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
        data.workflows.remove(&instance_name);
        tracing::warn!("Failed to persist onboarding workflow run: {e:#}");
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to start onboarding workflow: {e}"),
        });
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let onboarding_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let workflows = data.workflows.clone();
    *onboarding_state.invited_peers.write().await = peer_ids.clone();
    let party_credentials = data.party_credentials.clone();
    let auth_lock = data.auth.clone();
    let last_seen = data.last_seen.clone();
    let instance_for_task = instance_name.clone();

    // See start_dars below for the rationale: abort_handle, status, and error
    // are flipped under simultaneously-held locks so a concurrent
    // /onboarding/cancel can never observe "status=InProgress + abort_handle=None"
    // and bail.
    let mut abort_guard = onboarding_state.abort_handle.lock().await;
    let mut status_guard = onboarding_state.status.write().await;
    let mut error_guard = onboarding_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send invites to selected peers before starting coordinator workflow
        let invite_result = send_onboarding_invites(
            &config,
            &db,
            &peer_ids,
            &party_id_prefix,
            &instance_for_task,
        )
        .await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send onboarding invites: {e}");
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
            last_seen,
            instance_for_coord,
        )
        .await;

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
pub async fn get_onboarding_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Onboarding).await)
}

/// Interpret a peer's reply to a workflow invite.
///
/// A `Busy` reply means the peer raced into another workflow between our
/// pre-flight health check and this invite. Treat it as a hard error so the
/// coordinator aborts fast — the spawning task marks the run Failed with a clear
/// reason — instead of waiting on a peer that will never join. Any other
/// non-`Ack` reply, or an unparseable one, is logged but tolerated (an older
/// peer may not Ack).
fn interpret_invite_reply(peer_id: &CantonId, kind: &str, response: &[u8]) -> Result {
    match Message::from_bytes(response) {
        Ok(msg) if msg.msg_type == MessageType::Ack => {
            tracing::info!("Peer {peer_id} acknowledged {kind} invite");
            Ok(())
        }
        Ok(msg) if msg.msg_type == MessageType::Busy => {
            anyhow::bail!(
                "Peer {peer_id} is already participating in another workflow; \
                 aborting {kind} before it starts"
            )
        }
        Ok(msg) => {
            tracing::warn!(
                "Peer {peer_id} responded with {msg_type:?} instead of Ack to {kind} invite",
                msg_type = msg.msg_type
            );
            Ok(())
        }
        Err(_) => {
            tracing::warn!("Peer {peer_id} sent an unparseable {kind} invite reply");
            Ok(())
        }
    }
}

/// Send onboarding invites to selected peers using Noise protocol
async fn send_onboarding_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    party_id_prefix: &str,
    instance_name: &str,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let payload = OnboardingInvitePayload {
        prefix: party_id_prefix.to_string(),
        participants: peer_ids.to_vec(),
        workflow_instance: Some(instance_name.to_string()),
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
                interpret_invite_reply(peer_id, "onboarding", &response)?;
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
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse)
    )
)]
#[post("/contracts")]
pub async fn start_contracts(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ContractsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
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

    // Scope to the party's actual signers, not every peer this node knows
    // about. A dec party can be a strict subset of the configured mesh (see
    // start_onboarding), so inviting/waiting for non-member peers would pull
    // outsiders into the deployment or stall WaitingForPeers on a peer that
    // never joins. Keep only configured peers that are this party's
    // participants, minus self.
    let participant_set: HashSet<CantonId> = body.participant_ids.iter().cloned().collect();
    let contracts_invitees: Vec<CantonId> = match data.db.get_all_peers().await {
        Ok(peers) => peers
            .into_iter()
            .map(|p| p.participant_id)
            .filter(|p| p != data.config.participant_id() && participant_set.contains(p))
            .collect(),
        Err(e) => {
            tracing::warn!("Failed to load peers for cancel-invite tracking: {e}");
            Vec::new()
        }
    };

    let instance_name_for_run = contracts_config.instance_name.clone();
    let instance = WorkflowInstance::new(
        instance_name_for_run.clone(),
        WorkflowKind::Contracts,
        WorkflowRole::Coordinator,
    );
    let contracts_state = &instance.http;
    // Register FIRST — atomic dedup before persist (a racing same-instance
    // request can't upsert/clobber the in-flight run's row).
    if !data.workflows.insert(instance.clone()) {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "A workflow with instance {} is already running",
                instance.instance_name
            ),
        });
    }

    // Persist; unregister on failure so a never-started run can't leak.
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
        data.workflows.remove(&instance_name_for_run);
        tracing::warn!("Failed to persist contracts workflow run: {e:#}");
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to start contracts workflow: {e}"),
        });
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let workflow_auth = data.auth.read().await.clone();
    let auth_lock = data.auth.clone();
    let contracts_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let workflows = data.workflows.clone();
    let party_credentials = data.party_credentials.clone();
    let last_seen = data.last_seen.clone();
    let contracts_invitees_for_invites = contracts_invitees.clone();
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
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send invites to this party's members before starting coordinator workflow
        let invite_result = send_contracts_invites(
            &config,
            &db,
            &contracts_config,
            &contracts_invitees_for_invites,
        )
        .await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send contracts invites: {e}");
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
            last_seen,
            instance_for_coord,
        )
        .await;

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
pub async fn get_contracts_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Contracts).await)
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
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "peer_ids must contain at least one peer".to_string(),
        });
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
        dar_files: body.dar_files.clone(),
        instance_name: instance_name.clone(),
        peer_ids: body.peer_ids.clone(),
    };

    // Register FIRST — atomic dedup before persist (no clobber on a race).
    if !data.workflows.insert(instance.clone()) {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "A workflow with instance {} is already running",
                instance.instance_name
            ),
        });
    }

    // Persist; unregister on failure so a never-started run can't leak.
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
        data.workflows.remove(&instance_name);
        tracing::warn!("Failed to persist dars workflow run: {e:#}");
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to start DARs workflow: {e}"),
        });
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let dars_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let workflows = data.workflows.clone();
    let last_seen = data.last_seen.clone();
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
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send invites to selected peers before starting coordinator workflow
        let dar_filenames: Vec<String> = dars_config
            .dar_files
            .iter()
            .map(|f| f.filename.clone())
            .collect();
        let invite_result = send_dars_invites(
            &config,
            &db,
            &peer_ids,
            &dar_filenames,
            &dars_config.instance_name,
        )
        .await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send DARs invites: {e}");
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
            last_seen,
            instance_for_coord,
        )
        .await;

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
pub async fn get_dars_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Dars).await)
}

/// Send DARs invites to selected peers using Noise protocol
async fn send_dars_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    dar_filenames: &[String],
    instance_name: &str,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let payload = DarsInvitePayload {
        dar_filenames: dar_filenames.to_vec(),
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

/// Shared cancel logic. All four workflow types use HttpWorkflowState<WorkflowProgress>.
async fn cancel_workflow_state(
    data: &web::Data<AppState>,
    label: &str,
    kind: WorkflowKind,
) -> HttpResponse {
    // Pick the coordinator run of this kind deterministically (lowest
    // instance_name) — `snapshot()` is HashMap-ordered, so with concurrent
    // same-kind runs an arbitrary pick would cancel a different instance each
    // call. Per-instance cancel is available via `/workflows/{instance}/cancel`.
    let mut candidates: Vec<_> = data
        .workflows
        .snapshot()
        .into_iter()
        .filter(|i| i.kind == kind && i.role == WorkflowRole::Coordinator)
        .collect();
    candidates.sort_by(|a, b| a.instance_name.cmp(&b.instance_name));
    let Some(instance) = candidates.into_iter().next() else {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("No {label} workflow in progress"),
        });
    };
    let state = &instance.http;
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

    let invitees = state.invited_peers.read().await.clone();
    if !invitees.is_empty()
        && let Err(e) =
            send_cancel_invites(&data.config, &data.db, &invitees, &instance.instance_name).await
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
    if let Err(e) = mark_run_status(
        &data.db,
        &instance.instance_name,
        WorkflowProgress::Cancelled,
        None,
    )
    .await
    {
        tracing::warn!(
            "Failed to flip workflow_runs row {} to cancelled: {e:#}",
            instance.instance_name
        );
    }

    tracing::info!("{label} workflow {} cancelled", instance.instance_name);
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
pub async fn cancel_onboarding(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Onboarding", WorkflowKind::Onboarding).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/kick/cancel")]
pub async fn cancel_kick(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Kick", WorkflowKind::Kick).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/contracts/cancel")]
pub async fn cancel_contracts(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Contracts", WorkflowKind::Contracts).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/dars/cancel")]
pub async fn cancel_dars(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "DARs", WorkflowKind::Dars).await
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
            enrich_from_config_json(&mut r);
            r
        })
        .collect();

    HttpResponse::Ok().json(WorkflowRunsResponse { runs: resolved })
}

/// Pull `prefix` + `participants` out of the run's `config_json` and lift
/// them onto the response struct so the frontend can show them without
/// parsing JSON blobs. Coordinator configs spell the prefix field
/// `party_id_prefix` (e.g. `OnboardingConfig`) while the peer-side payload
/// uses `prefix`; we accept either. For participants we fall back to
/// `expected_peers` when the config doesn't carry a list of its own — that
/// way Kick / Contracts / Dars runs (whose configs don't include a peer
/// list) still surface their participants.
fn enrich_from_config_json(run: &mut WorkflowRun) {
    // A contract entry in a coordinator-side `ContractsConfig`. We only need
    // its human-readable `name` for the card's "Packages" row.
    #[derive(serde::Deserialize)]
    struct ContractNameShape {
        #[serde(default)]
        name: String,
    }
    // A DAR entry in a coordinator-side `DarsConfig`.
    #[derive(serde::Deserialize)]
    struct DarFileShape {
        #[serde(default)]
        filename: String,
    }
    #[derive(serde::Deserialize)]
    struct ConfigShape {
        #[serde(default)]
        prefix: Option<String>,
        #[serde(default)]
        party_id_prefix: Option<String>,
        #[serde(default)]
        participants: Vec<CantonId>,
        // Kick configs only.
        #[serde(default)]
        new_threshold: Option<i32>,
        #[serde(default)]
        previous_threshold: Option<i32>,
        #[serde(default)]
        participant_id: Option<CantonId>,
        // Contracts: `package_names` is the peer's flat list (from the invite);
        // `contracts[].name` is the coordinator's `ContractsConfig`.
        #[serde(default)]
        package_names: Vec<String>,
        #[serde(default)]
        contracts: Vec<ContractNameShape>,
        // Dars: `dar_filenames` is the peer's flat list (from the invite);
        // `dar_files[].filename` is the coordinator's `DarsConfig`.
        #[serde(default)]
        dar_filenames: Vec<String>,
        #[serde(default)]
        dar_files: Vec<DarFileShape>,
    }
    if let Ok(shape) = serde_json::from_str::<ConfigShape>(&run.config_json) {
        let prefix = shape.prefix.or(shape.party_id_prefix);
        if let Some(p) = prefix
            && !p.is_empty()
        {
            run.prefix = Some(p);
        }
        if !shape.participants.is_empty() {
            run.participants = shape.participants;
        }
        run.new_threshold = shape.new_threshold;
        // Only surface a previous threshold when an older client actually
        // sent one (it defaults to 0); 0 means "unknown", render as new-only.
        run.previous_threshold = shape.previous_threshold.filter(|t| *t > 0);
        run.kicked_participant = shape.participant_id;
        // Package names: peer's flat list wins; otherwise derive from the
        // coordinator's contract definitions. Both converge to the same set.
        if !shape.package_names.is_empty() {
            run.package_names = shape.package_names;
        } else if !shape.contracts.is_empty() {
            run.package_names = shape
                .contracts
                .into_iter()
                .map(|c| c.name)
                .filter(|n| !n.is_empty())
                .collect();
        }
        // DAR filenames: same peer-flat-list-vs-coordinator-config convergence.
        if !shape.dar_filenames.is_empty() {
            run.dar_filenames = shape.dar_filenames;
        } else if !shape.dar_files.is_empty() {
            run.dar_filenames = shape
                .dar_files
                .into_iter()
                .map(|d| d.filename)
                .filter(|n| !n.is_empty())
                .collect();
        }
    }
    // Fallback: if config_json didn't expose a participants list (e.g. Kick /
    // Contracts / Dars), surface the run's `expected_peers` instead so the
    // card still shows who was involved.
    if run.participants.is_empty() && !run.expected_peers.is_empty() {
        run.participants = run.expected_peers.clone();
    }
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
    if let Err(e) = send_retry_workflow(
        &data.config,
        &data.db,
        &run.expected_peers,
        &run.instance_name,
    )
    .await
    {
        tracing::warn!("Failed to broadcast RetryWorkflow: {e:#}");
    }
    respawn_coordinator(
        data.db.clone(),
        data.config.clone(),
        &run,
        data.workflows.clone(),
        data.auth.clone(),
        data.last_seen.clone(),
    )
    .await;

    HttpResponse::Ok().json(MessageResponse {
        message: format!(
            "Retrying workflow {instance_name} from step {}",
            run.current_step
        ),
    })
}

/// Look for an existing decentralized party whose human-readable prefix
/// equals `prefix`. Returns the matching `party_id` if found — used by the
/// onboarding pre-flight to refuse duplicate-prefix runs.
async fn find_party_with_prefix(db: &SqlitePool, prefix: &str) -> Result<Option<String>> {
    use crate::db::schema::SchemaRead;

    let parties = db.get_dec_parties_by_prefix(prefix).await?;
    Ok(parties.into_iter().next().map(|p| p.party_id))
}

/// `instance` is the cancelled run's `instance_name`, stamped onto the message
/// so peers cancel only that run's invite/peer-run — not every run they hold
/// from this coordinator (which may have other concurrent runs in flight).
async fn send_cancel_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    instance: &str,
) -> Result {
    broadcast_simple_message(
        config,
        db,
        peer_ids,
        Message::new_empty(MessageType::CancelInvite).with_instance(instance),
        "CancelInvite",
    )
    .await
}

/// Best-effort: notify previously-invited peers that the coordinator is
/// retrying the workflow so they flip their Failed row back to InProgress
/// and re-spin `start_peer`. `instance` scopes the retry to this run's peer
/// rows (see `send_cancel_invites`).
async fn send_retry_workflow(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    instance: &str,
) -> Result {
    broadcast_simple_message(
        config,
        db,
        peer_ids,
        Message::new_empty(MessageType::RetryWorkflow).with_instance(instance),
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

/// Send contracts invites to this party's members (`invitees`) over Noise.
async fn send_contracts_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    contracts_config: &workflow::ContractsConfig,
    invitees: &[CantonId],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
    // Carry the dec party, member set, and package names so the peer card
    // renders the same rich summary the coordinator shows (mirrors the Kick
    // invite). Skip empty names, then sort+dedup — `dedup` only removes
    // adjacent duplicates, and multiple contracts can share a package name.
    let mut package_names: Vec<String> = contracts_config
        .contracts
        .iter()
        .map(|c| c.name.clone())
        .filter(|n| !n.is_empty())
        .collect();
    package_names.sort();
    package_names.dedup();
    let payload = ContractsInvitePayload {
        dec_party_id: contracts_config.decentralized_party_id.clone(),
        participants: invitees.to_vec(),
        package_names,
        workflow_instance: Some(contracts_config.instance_name.clone()),
    };
    let payload_bytes = serde_json::to_vec(&payload).context("encode ContractsInvitePayload")?;
    let invite_message = Message::new(MessageType::InviteContracts, payload_bytes);

    for peer in &network_config.peers {
        if peer.participant_id == *current_participant_id {
            continue;
        }

        // Only this party's members (see start_contracts) — never every
        // configured peer.
        if !invitee_set.contains(&peer.participant_id) {
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
                interpret_invite_reply(&peer.participant_id, "contracts", &response)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_reply_aborts_on_busy_only() -> anyhow::Result<()> {
        let peer = CantonId::parse(
            "participant::12200ad4539c269a7b13af6806fb2ee326e7c0d7233fa6144004c416502a2c73fb0b",
        )?;

        // Ack is the happy path.
        let ack = Message::new_empty(MessageType::Ack).to_bytes();
        assert!(interpret_invite_reply(&peer, "onboarding", &ack).is_ok());

        // Busy aborts so the coordinator fails fast instead of waiting forever.
        let busy = Message::new(MessageType::Busy, b"Onboarding".to_vec()).to_bytes();
        assert!(interpret_invite_reply(&peer, "onboarding", &busy).is_err());

        // Any other (parseable) reply is tolerated — older peers may not Ack.
        let other = Message::new_empty(MessageType::Wait).to_bytes();
        assert!(interpret_invite_reply(&peer, "onboarding", &other).is_ok());

        Ok(())
    }

    fn test_cid(prefix: &str) -> anyhow::Result<CantonId> {
        let ns = format!("1220{:0>64}", "a");
        CantonId::parse(&format!("{prefix}::{ns}"))
    }

    fn enrich_run(config_json: &str, expected_peers: Vec<CantonId>) -> WorkflowRun {
        WorkflowRun {
            instance_name: "t".to_string(),
            kind: WorkflowKind::Contracts,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: "Active".to_string(),
            step_index: 0,
            step_total: 5,
            config_json: config_json.to_string(),
            coordinator_pubkey: None,
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers,
            completed_peers: Vec::new(),
            dec_party_id: None,
            prefix: None,
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            package_names: Vec::new(),
            dar_filenames: Vec::new(),
            error: None,
            dismissed: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn enrich_contracts_coordinator_surfaces_packages() -> anyhow::Result<()> {
        let peers = vec![test_cid("node1")?, test_cid("node2")?];
        let mut run = enrich_run(
            r#"{"contracts":[{"name":"Governance Core"},{"name":"Token Custody"}]}"#,
            peers.clone(),
        );
        enrich_from_config_json(&mut run);
        assert_eq!(run.package_names, vec!["Governance Core", "Token Custody"]);
        // Member set comes from expected_peers when config_json has no list.
        assert_eq!(run.participants, peers);
        Ok(())
    }

    #[test]
    fn enrich_contracts_peer_surfaces_packages_and_participants() -> anyhow::Result<()> {
        let peers = vec![test_cid("node1")?, test_cid("node2")?];
        let peers_json = serde_json::to_string(&peers)?;
        let mut run = enrich_run(
            &format!(r#"{{"package_names":["Governance Core"],"participants":{peers_json}}}"#),
            Vec::new(),
        );
        enrich_from_config_json(&mut run);
        assert_eq!(run.package_names, vec!["Governance Core"]);
        assert_eq!(run.participants, peers);
        Ok(())
    }

    #[test]
    fn enrich_dars_surfaces_filenames_without_dec_party() -> anyhow::Result<()> {
        let mut run = enrich_run(
            r#"{"dar_files":[{"filename":"app.dar"},{"filename":"lib.dar"}]}"#,
            Vec::new(),
        );
        enrich_from_config_json(&mut run);
        assert_eq!(run.dar_filenames, vec!["app.dar", "lib.dar"]);
        assert!(run.dec_party_id.is_none());
        Ok(())
    }

    #[test]
    fn contracts_invite_payload_roundtrips_with_defaults() -> anyhow::Result<()> {
        let payload = ContractsInvitePayload {
            dec_party_id: test_cid("dec")?,
            participants: vec![test_cid("node1")?],
            package_names: vec!["Governance Core".to_string()],
            workflow_instance: Some("dec-contracts-1".to_string()),
        };
        let bytes = serde_json::to_vec(&payload)?;
        let back: ContractsInvitePayload = serde_json::from_slice(&bytes)?;
        assert_eq!(back.package_names, vec!["Governance Core"]);
        assert_eq!(back.participants.len(), 1);
        // A minimal payload (only the required dec party) still decodes, so an
        // older coordinator stays compatible.
        let minimal: ContractsInvitePayload =
            serde_json::from_str(&format!(r#"{{"dec_party_id":"{}"}}"#, test_cid("dec")?))?;
        assert!(minimal.participants.is_empty());
        assert!(minimal.package_names.is_empty());
        Ok(())
    }
}
