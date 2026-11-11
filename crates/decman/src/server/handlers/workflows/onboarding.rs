//! The onboarding workflow: start, status, invites and the peer mesh check.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use sqlx::SqlitePool;

use crate::server::handlers::parties::resolve_owner_keys_from_peers;
use crate::{
    canton_id::{CantonId, validate_party_id_prefix},
    config::{NetworkConfig, NodeConfig},
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    server::{
        AppState,
        middleware::require_admin,
        types::{
            ErrorResponse, MissingEdgeKind, MissingPeerEdge, OnboardingInvitePayload,
            OnboardingMeshErrorResponse, OnboardingRequest, OnboardingResponse, OnboardingStatus,
            WorkflowGuard, WorkflowInstance, WorkflowKind, WorkflowResponse, WorkflowRole,
            WorkflowStatusResponse,
        },
    },
    workflow::{self, OnboardingStep, thresholds::check_confirmation_threshold},
};

use super::{
    preflight::{find_party_with_prefix, format_incompatible_peers, preflight_incompatible_peers},
    runs::{kind_status, mark_run_completed, mark_run_failed, register_and_persist},
};

// ============================================================================
// Onboarding Workflow
// ============================================================================

/// Start an onboarding workflow to create a new decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = OnboardingRequest,
    responses(
        (status = 202, description = "Onboarding workflow started", body = WorkflowResponse),
        (status = 400, description = "Invalid party prefix, threshold or confirmation threshold", body = ErrorResponse),
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

    // If the operator set an explicit initial threshold, bound it by the owner
    // count (invited peers + this coordinator). Omitted => the coordinator uses
    // the default majority algorithm once the owner set is resolved.
    let owner_count = peer_ids.len() as i32 + 1;
    if let Some(t) = body.threshold
        && !(1..=owner_count).contains(&t)
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "threshold must be between 1 and {owner_count} (invited peers + this node); got {t}"
            ),
        });
    }
    // Every owner's node also hosts the party, so the same count bounds the
    // confirmation threshold.
    if let Some(confirmation) = body.confirmation_threshold
        && let Err(e) = check_confirmation_threshold(confirmation, owner_count as usize)
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: e.to_string(),
        });
    }

    let onboarding_config = workflow::OnboardingConfig::new(
        party_id_prefix.clone(),
        instance_name.clone(),
        body.threshold,
        body.confirmation_threshold,
    );

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

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible = preflight_incompatible_peers(&data.config, &data.db, &peer_ids).await;
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }

    // Onboarding's instance_name is deterministic (`{prefix}-creation`), so a
    // racing same-prefix request is realistic — register_and_persist's registry
    // insert rejects it before the upsert can clobber the in-flight run's row.
    if let Err(resp) = register_and_persist(
        &data,
        &instance,
        OnboardingStep::WaitingForPeers,
        &onboarding_config,
        &peer_ids,
        None,
    )
    .await
    {
        return resp;
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let onboarding_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    *onboarding_state.invited_peers.write().await = peer_ids.clone();
    let party_credentials = data.party_credentials.clone();
    let auth_lock = data.auth.clone();
    let discovery_gate = super::DiscoveryGate::of(&data);
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
            onboarding_config.threshold,
            onboarding_config.confirmation_threshold,
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
            mark_run_failed(&db, kind, &instance_for_task, &msg).await;
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
            None, // No add-party config
            None, // No change-threshold config
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
                mark_run_completed(&db, kind, &instance_for_task).await;
                // Operator configures party credentials via the Party Configuration
                // dialog; auto-saving placeholders pollutes the auth registry.

                // Refresh dec_party cache in background
                let bg_config = config.clone();
                let bg_db = db.clone();
                let bg_auth = auth_lock.clone();
                let bg_creds = party_credentials.clone();
                let bg_gate = discovery_gate.clone();
                tokio::spawn(async move {
                    let auth = bg_auth.read().await.clone();
                    let creds = bg_creds.read().await.clone();
                    // Through the shared gate: a direct discovery slips the
                    // concurrency bound and races the cache write against a
                    // request for the same prefix.
                    match super::discover_and_cache(
                        &bg_gate,
                        &bg_config,
                        &bg_db,
                        "",
                        auth,
                        &creds,
                        Default::default(),
                    )
                    .await
                    {
                        super::Discovery::Done(resp) => {
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
                        super::Discovery::InFlight
                        | super::Discovery::AtCapacity
                        | super::Discovery::Superseded => {
                            tracing::debug!("Post-onboarding refresh skipped: already running");
                        }
                        super::Discovery::Failed(e) => {
                            tracing::warn!("Failed to refresh parties after onboarding: {e}");
                        }
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
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
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
        instance_name,
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
/// Current peers accept invites unconditionally (workflows are multi-instance
/// now — see the invite arm in `server::mod`), so a current peer always replies
/// `Ack`, and a pre-`Ack` build is refused by the version gate before it can
/// reach an invite. The `Busy` arm is therefore a defensive safety net no
/// current-version peer triggers: were a peer ever to report `Busy`, treating it
/// as a hard error makes the coordinator fail fast (the spawning task marks the
/// run Failed with a clear reason) instead of waiting on a peer that will never
/// join. Any other non-`Ack` reply, or an unparseable one, is logged but
/// tolerated.
pub(super) fn interpret_invite_reply(peer_id: &CantonId, kind: &str, response: &[u8]) -> Result {
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
    threshold: Option<i32>,
    confirmation_threshold: Option<i32>,
    instance_name: &str,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    // Show the threshold the coordinator will actually use: the operator's
    // choice, or the same majority default it falls back to (owners = invited
    // peers + this coordinator).
    let owners = peer_ids.len() + 1;
    let effective_threshold = threshold.unwrap_or_else(|| owners.div_ceil(2).max(1) as i32);

    let payload = OnboardingInvitePayload {
        prefix: party_id_prefix.to_string(),
        participants: peer_ids.to_vec(),
        threshold: Some(effective_threshold),
        confirmation_threshold,
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

        // Busy is a defensive net no current-version peer triggers (peers Ack
        // unconditionally and pre-Ack builds are version-gated out); were it ever
        // sent, it still aborts so the coordinator fails fast instead of waiting.
        let busy = Message::new(MessageType::Busy, b"Onboarding".to_vec()).to_bytes();
        assert!(interpret_invite_reply(&peer, "onboarding", &busy).is_err());

        // Any other (parseable) reply is tolerated — older peers may not Ack.
        let other = Message::new_empty(MessageType::Wait).to_bytes();
        assert!(interpret_invite_reply(&peer, "onboarding", &other).is_ok());

        Ok(())
    }
}
