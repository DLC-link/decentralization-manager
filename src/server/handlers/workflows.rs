use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use sqlx::SqlitePool;

use super::parties::{
    fetch_decentralized_parties, resolve_owner_keys_from_peers, store_parties_to_db,
};
use crate::{
    config::{NetworkConfig, NodeConfig},
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    participant_id::CantonId,
    server::{
        AppState,
        middleware::require_admin,
        types::{
            ContractsRequest, DarsInvitePayload, DarsRequest, ErrorResponse, HttpWorkflowState,
            KickRequest, KickResponse, KickStatus, ListenerPauseGuard, MessageResponse,
            MissingEdgeKind, MissingPeerEdge, OnboardingInvitePayload,
            OnboardingMeshErrorResponse, OnboardingRequest, OnboardingResponse, OnboardingStatus,
            SuccessResponse, WorkflowProgress, WorkflowResponse, WorkflowStatusResponse,
        },
    },
    workflow,
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

    // Parse IDs first for validation
    let decentralized_party_id = match CantonId::parse(&body.decentralized_party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Invalid decentralized_party_id: {e}"),
            });
        }
    };

    let participant_id = match CantonId::parse(&body.participant_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Invalid participant_id: {e}"),
            });
        }
    };

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
        .get_dec_party_participant_owner_key(
            &decentralized_party_id.to_string(),
            &participant_id.to_string(),
        )
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

    // Compute peers we're going to invite — every peer except self + the kicked participant.
    // Done before the InProgress flip so a concurrent /kick/cancel cannot observe
    // InProgress while we're still preparing.
    let invitees: Vec<CantonId> = match data.db.get_all_peers().await {
        Ok(peers) => peers
            .into_iter()
            .map(|p| p.participant_id)
            .filter(|p| p != data.config.participant_id() && p != &participant_id)
            .collect(),
        Err(e) => {
            tracing::warn!("Failed to load peers for cancel-invite tracking: {e}");
            Vec::new()
        }
    };

    // Flip status, write invited_peers, spawn, and stash the abort handle in one
    // go. cancel_workflow_state only acts when abort_handle is Some, so as long as
    // these run without intervening cancel-visible state, the race is closed.
    {
        let mut status = kick_state.status.write().await;
        *status = KickStatus::InProgress;
    }
    {
        let mut error = kick_state.error.write().await;
        *error = None;
    }
    *kick_state.invited_peers.write().await = invitees;

    // Spawn the kick workflow in the background
    let config = data.config.clone();
    let db = data.db.clone();
    let kick_state_clone = kick_state.get_ref().clone();
    let new_threshold = body.new_threshold;
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();

    let join_handle = tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send kick invites to all peers before starting coordinator workflow
        let invite_result = send_kick_invites(&config, &db, &participant_id).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send kick invites: {e}");
            guard.resume().await;
            let mut status = kick_state_clone.status.write().await;
            let mut error = kick_state_clone.error.write().await;
            *status = KickStatus::Failed;
            *error = Some(format!("Failed to send invites: {e}"));
            return;
        }

        // Give peers time to start their attestor workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let instance_name = format!("{}-kick-{timestamp}", decentralized_party_id.prefix);
        let kick_config = workflow::KickConfig::new(
            decentralized_party_id,
            participant_id,
            namespace_fingerprint,
            new_threshold,
            instance_name,
        );

        let result = workflow::start_coordinator(
            config,
            db,
            workflow::WorkflowType::Kick,
            None, // No onboarding config
            Some(kick_config),
            None, // No contracts config
            None, // No dars config
            None, // No auth registry for kick
        )
        .await;

        guard.resume().await;

        let mut status = kick_state_clone.status.write().await;
        let mut error = kick_state_clone.error.write().await;

        match result {
            Ok(_) => {
                *status = KickStatus::Completed;
                tracing::info!("Kick workflow completed successfully");
            }
            Err(e) => {
                *status = KickStatus::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Kick workflow failed: {e}");
            }
        }
    });
    *kick_state.abort_handle.lock().await = Some(join_handle.abort_handle());

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
    data: web::Data<AppState>,
    onboarding_state: web::Data<Arc<OnboardingWorkflowState>>,
    body: web::Json<OnboardingRequest>,
) -> impl Responder {
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
    // for attestor connections that can never be established.
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

    // Pre-spawn handles + record invitees BEFORE we flip status to InProgress, so a
    // concurrent /onboarding/cancel cannot observe InProgress while abort_handle is
    // still None (cancel_workflow_state requires Some(abort_handle) to proceed).
    let config = data.config.clone();
    let db = data.db.clone();
    let onboarding_state_clone = onboarding_state.get_ref().clone();
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();
    let party_id_prefix = body.party_id_prefix.clone();
    let peer_ids = body.peer_ids.clone();
    *onboarding_state.invited_peers.write().await = peer_ids.clone();
    let party_credentials = data.party_credentials.clone();
    let auth_lock = data.auth.clone();

    {
        let mut status = onboarding_state.status.write().await;
        *status = OnboardingStatus::InProgress;
    }
    {
        let mut error = onboarding_state.error.write().await;
        *error = None;
    }

    let join_handle = tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send invites to selected peers before starting coordinator workflow
        let invite_result =
            send_onboarding_invites(&config, &db, &peer_ids, &party_id_prefix).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send onboarding invites: {e}");
            guard.resume().await;
            let mut status = onboarding_state_clone.status.write().await;
            let mut error = onboarding_state_clone.error.write().await;
            *status = OnboardingStatus::Failed;
            *error = Some(format!("Failed to send invites: {e}"));
            return;
        }

        // Give peers time to start their attestor workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let instance_name = format!("{party_id_prefix}-creation");
        let onboarding_config =
            workflow::OnboardingConfig::new(party_id_prefix.clone(), instance_name);

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

        let mut status = onboarding_state_clone.status.write().await;
        let mut error = onboarding_state_clone.error.write().await;

        match result {
            Ok(_) => {
                *status = OnboardingStatus::Completed;
                tracing::info!("Onboarding workflow completed successfully");
                // Operator configures party credentials via the Party Configuration dialog; auto-saving placeholders pollutes the auth registry.

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
                                let party_id = party.party_id.to_string();
                                for p in &party.participants {
                                    let uid = p.participant_uid.to_string();
                                    match bg_db
                                        .get_dec_party_participant_owner_key(&party_id, &uid)
                                        .await
                                    {
                                        Ok(Some(_)) => {} // resolved
                                        Ok(None) => tracing::warn!(
                                            party_id = %party_id,
                                            participant_uid = %uid,
                                            "Participant owner_key unresolved after onboarding"
                                        ),
                                        Err(e) => tracing::warn!(
                                            party_id = %party_id,
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
                *status = OnboardingStatus::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Onboarding workflow failed: {e}");
            }
        }
    });
    *onboarding_state.abort_handle.lock().await = Some(join_handle.abort_handle());

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
        participants: peer_ids.iter().map(|id| id.to_string()).collect(),
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
    data: web::Data<AppState>,
    contracts_state: web::Data<Arc<ContractsWorkflowState>>,
    body: web::Json<ContractsRequest>,
) -> impl Responder {
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
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
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

    // Pre-spawn handles BEFORE we flip status to InProgress, so a concurrent
    // /contracts/cancel cannot observe InProgress while abort_handle is still
    // None (cancel_workflow_state requires Some(abort_handle) to proceed).
    let config = data.config.clone();
    let db = data.db.clone();
    let workflow_auth = data.auth.read().await.clone();
    let auth_lock = data.auth.clone();
    let contracts_state_clone = contracts_state.get_ref().clone();
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();
    let party_credentials = data.party_credentials.clone();
    *contracts_state.invited_peers.write().await = contracts_invitees;

    {
        let mut status = contracts_state.status.write().await;
        *status = WorkflowProgress::InProgress;
    }
    {
        let mut error = contracts_state.error.write().await;
        *error = None;
    }

    let join_handle = tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send invites to all peers before starting coordinator workflow
        let invite_result = send_contracts_invites(&config, &db).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send contracts invites: {e}");
            guard.resume().await;
            let mut status = contracts_state_clone.status.write().await;
            let mut error = contracts_state_clone.error.write().await;
            *status = WorkflowProgress::Failed;
            *error = Some(format!("Failed to send invites: {e}"));
            return;
        }

        // Give peers time to start their attestor workflows
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

        let mut status = contracts_state_clone.status.write().await;
        let mut error = contracts_state_clone.error.write().await;

        match result {
            Ok(_) => {
                *status = WorkflowProgress::Completed;
                tracing::info!("Contracts workflow completed successfully");

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
                *status = WorkflowProgress::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Contracts workflow failed: {e}");
            }
        }
    });
    *contracts_state.abort_handle.lock().await = Some(join_handle.abort_handle());

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
    data: web::Data<AppState>,
    body: web::Json<DarsRequest>,
) -> impl Responder {
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
    data: web::Data<AppState>,
    dars_state: web::Data<Arc<DarsWorkflowState>>,
    body: web::Json<DarsRequest>,
) -> impl Responder {
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
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let instance_name = format!("dars-distribute-{timestamp}");
    let dars_config = workflow::DarsConfig {
        dar_files: body.dar_files.clone(),
        instance_name,
        peer_ids: body.peer_ids.clone(),
    };

    // Pre-spawn handles + record invitees BEFORE we flip status to InProgress, so a
    // concurrent /dars/cancel cannot observe InProgress while abort_handle is still
    // None (cancel_workflow_state requires Some(abort_handle) to proceed).
    let config = data.config.clone();
    let db = data.db.clone();
    let dars_state_clone = dars_state.get_ref().clone();
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();
    let peer_ids = body.peer_ids.clone();
    *dars_state.invited_peers.write().await = peer_ids.clone();

    {
        let mut status = dars_state.status.write().await;
        *status = WorkflowProgress::InProgress;
    }
    {
        let mut error = dars_state.error.write().await;
        *error = None;
    }

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
            return;
        }

        // Give peers time to start their attestor workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            db,
            workflow::WorkflowType::Dars,
            None, // No onboarding config
            None, // No kick config
            None, // No contracts config
            Some(dars_config),
            None, // No auth
        )
        .await;

        guard.resume().await;

        let mut status = dars_state_clone.status.write().await;
        let mut error = dars_state_clone.error.write().await;

        match result {
            Ok(_) => {
                *status = WorkflowProgress::Completed;
                tracing::info!("DARs distribution workflow completed successfully");
            }
            Err(e) => {
                *status = WorkflowProgress::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("DARs distribution workflow failed: {e}");
            }
        }
    });
    *dars_state.abort_handle.lock().await = Some(join_handle.abort_handle());

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
    cancel_workflow_state(state.get_ref(), &data, "Onboarding").await
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
    cancel_workflow_state(state.get_ref(), &data, "Kick").await
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
    cancel_workflow_state(state.get_ref(), &data, "Contracts").await
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
    cancel_workflow_state(state.get_ref(), &data, "DARs").await
}

/// Best-effort: notify previously-invited peers that the workflow is cancelled
/// so they can drop the matching pending invitation.
async fn send_cancel_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let cancel_message = Message::new_empty(MessageType::CancelInvite);
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
            &cancel_message,
        )
        .await
        {
            tracing::warn!("Failed to send CancelInvite to {peer_id}: {e}");
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
