use std::{collections::HashMap, time::Duration};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use crate::{
    config::Peer,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    noise::client::NoiseClient,
    server::{
        AppState, mark_failed_via_pool,
        middleware::require_admin,
        types::{
            DeclineInvitationPayload, ErrorResponse, InvitationActionRequest, MessageResponse,
            PeerJob, PendingInvitation, PendingInvitationsResponse, WorkflowKind, WorkflowProgress,
            WorkflowRole, WorkflowRun,
        },
    },
    workflow::{ContractsStep, DarsStep, KickStep, OnboardingStep, state::WorkflowStep},
};

async fn delete_persisted_invitation(data: &web::Data<AppState>, id: &str) {
    match data.db.begin_transaction().await {
        Ok(mut tx) => {
            if let Err(e) = tx.delete_pending_invitation(id).await {
                tracing::warn!("Failed to delete persisted invitation {id}: {e}");
            } else if let Err(e) = Commitable::commit(tx).await {
                tracing::warn!("Failed to commit invitation deletion {id}: {e}");
            }
        }
        Err(e) => tracing::warn!("Failed to begin tx to delete invitation {id}: {e}"),
    }
}

fn step_total_for(kind: WorkflowKind) -> i64 {
    match kind {
        WorkflowKind::Onboarding => OnboardingStep::step_total(),
        WorkflowKind::Kick => KickStep::step_total(),
        WorkflowKind::Contracts => ContractsStep::step_total(),
        WorkflowKind::Dars => DarsStep::step_total(),
    }
}

/// Insert the peer-side `workflow_runs` row for a freshly-accepted invite.
///
/// The synthetic instance_name is keyed on the coordinator's run instance
/// (`peer-<kind>-<coord_pubkey[..16]>-<workflow_instance>`): with concurrent
/// workflows a node can accept several same-kind invites from the SAME
/// coordinator back to back, and a timestamp suffix at seconds resolution
/// would collide — the second accept's upsert (ON CONFLICT(instance_name) DO
/// UPDATE) silently merging two distinct runs into one row, cross-wiring
/// their artefacts and statuses. Keying on the coordinator run makes the row
/// unique per run and stable across a re-sent invite of the same run (the
/// re-accept resumes the same row instead of duplicating it). Invites from
/// coordinators that predate instance routing carry no `workflow_instance`;
/// they fall back to the old timestamp suffix.
async fn insert_peer_run(
    data: &web::Data<AppState>,
    invitation: &PendingInvitation,
) -> Option<String> {
    let kind: WorkflowKind = invitation.invitation_type.into();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let pubkey_short =
        &invitation.coordinator_pubkey[..invitation.coordinator_pubkey.len().min(16)];
    let run_suffix = match invitation.workflow_instance.as_deref() {
        Some(coord_instance) if !coord_instance.is_empty() => coord_instance.to_string(),
        _ => now.to_string(),
    };
    let instance_name = format!(
        "peer-{}-{}-{}",
        kind.as_str().to_lowercase(),
        pubkey_short,
        run_suffix
    );
    let run = WorkflowRun {
        instance_name: instance_name.clone(),
        kind,
        role: WorkflowRole::Peer,
        status: WorkflowProgress::InProgress,
        // No per-step granularity on the peer side yet — coordinator drives
        // the protocol; we just track "I'm participating".
        current_step: "Active".to_string(),
        step_index: 0,
        step_total: step_total_for(kind),
        // We don't have the coordinator's full config; we have what they sent
        // in the invite payload. Persist that for forensic purposes / future
        // resume.
        config_json: serde_json::json!({
            "prefix": invitation.prefix,
            "participants": invitation.participants,
            "dar_filenames": invitation.dar_filenames,
            "participant_id": invitation.kicked_participant,
            "new_threshold": invitation.new_threshold,
            "previous_threshold": invitation.previous_threshold,
            "package_names": invitation.package_names,
        })
        .to_string(),
        coordinator_pubkey: Some(invitation.coordinator_pubkey.clone()),
        // The coordinator run this peer row belongs to — keys instance-scoped
        // CancelInvite/RetryWorkflow and peer-resume routing.
        coordinator_instance: invitation.workflow_instance.clone(),
        coordinator_name: None,
        // The participants list is the authoritative peer set carried in the
        // invite (all four kinds send one).
        expected_peers: invitation.participants.clone(),
        completed_peers: Vec::new(),
        // Kick + contracts invites carry the target dec party; others don't.
        dec_party_id: invitation.dec_party_id.clone(),
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

    let mut tx = match data.db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("peer run: begin_transaction failed: {e}");
            return None;
        }
    };
    if let Err(e) = tx.upsert_workflow_run(&run).await {
        tracing::warn!("peer run: upsert failed: {e}");
        return None;
    }
    if let Err(e) = Commitable::commit(tx).await {
        tracing::warn!("peer run: commit failed: {e}");
        return None;
    }
    Some(instance_name)
}

/// Get all pending invitations
#[utoipa::path(
    tag = "Invitations",
    responses(
        (status = 200, description = "Pending invitations", body = PendingInvitationsResponse)
    )
)]
#[get("/invitations")]
pub async fn get_invitations(data: web::Data<AppState>) -> impl Responder {
    let invitations = data.pending_invitations.read().await;

    // Resolve coordinator names from a single DB query
    let pubkey_to_name: HashMap<String, String> = data
        .db
        .get_all_peers()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.public_key, p.name))
        .collect();

    let invitations_with_names: Vec<PendingInvitation> = invitations
        .iter()
        .map(|inv| PendingInvitation {
            coordinator_name: pubkey_to_name.get(&inv.coordinator_pubkey).cloned(),
            ..inv.clone()
        })
        .collect();

    HttpResponse::Ok().json(PendingInvitationsResponse {
        invitations: invitations_with_names,
    })
}

/// Accept a pending invitation and trigger the workflow
#[utoipa::path(
    tag = "Invitations",
    request_body = InvitationActionRequest,
    responses(
        (status = 200, description = "Invitation accepted", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "Invitation not found", body = ErrorResponse)
    )
)]
#[post("/invitations/accept")]
pub async fn accept_invitation(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<InvitationActionRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let invitation = {
        let mut invitations = data.pending_invitations.write().await;
        let idx = invitations.iter().position(|i| i.id == body.id);
        match idx {
            Some(i) => invitations.remove(i),
            None => {
                return HttpResponse::NotFound().json(serde_json::json!({
                    "error": "Invitation not found"
                }));
            }
        }
    };

    delete_persisted_invitation(&data, &invitation.id).await;

    // Persist a peer-side workflow_runs row so the operator's feed shows
    // "I'm participating in <kind>" until completion.
    let Some(peer_instance) = insert_peer_run(&data, &invitation).await else {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "Failed to record accepted invitation"
        }));
    };

    // Enqueue a peer job. Carrying the coordinator's run instance
    // (`workflow_instance` from the invite) lets the peer route its commands
    // back to the right concurrent run; the single queue lets this node accept
    // many invites of any kind at once without racing over a shared slot.
    let job = PeerJob {
        kind: invitation.invitation_type.into(),
        instance_name: peer_instance,
        coordinator_instance: invitation.workflow_instance.clone().unwrap_or_default(),
        coordinator_pubkey: invitation.coordinator_pubkey.clone(),
    };
    tracing::info!(
        "Accepting {:?} invitation; enqueuing peer job {}",
        invitation.invitation_type,
        job.instance_name
    );
    let peer_instance_for_err = job.instance_name.clone();
    if data.peer_job_sender.send(job).is_err() {
        // The row was just persisted InProgress; without a listener nothing
        // will ever drive it, so mark it Failed instead of leaving a stale
        // in-progress card in the feed.
        mark_failed_via_pool(
            &data.db,
            &peer_instance_for_err,
            "Peer workflow listener unavailable",
        )
        .await;
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "Peer workflow listener unavailable"
        }));
    }

    HttpResponse::Ok().json(serde_json::json!({
        "message": "Invitation accepted"
    }))
}

/// Decline a pending invitation
#[utoipa::path(
    tag = "Invitations",
    request_body = InvitationActionRequest,
    responses(
        (status = 200, description = "Invitation declined", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "Invitation not found", body = ErrorResponse)
    )
)]
#[post("/invitations/decline")]
pub async fn decline_invitation(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<InvitationActionRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let invitation = {
        let mut invitations = data.pending_invitations.write().await;
        let idx = invitations.iter().position(|i| i.id == body.id);
        match idx {
            Some(i) => invitations.remove(i),
            None => {
                return HttpResponse::NotFound().json(serde_json::json!({
                    "error": "Invitation not found"
                }));
            }
        }
    };

    delete_persisted_invitation(&data, &invitation.id).await;

    // Best-effort: tell the coordinator we declined so they can fail their
    // matching in-progress run immediately instead of waiting until timeout.
    // If the Noise round-trip fails for any reason we still report success
    // to the operator — the local invitation is gone and the coordinator's
    // existing timeout/cancel paths cover the worst case.
    notify_coordinator_of_decline(&data, &invitation).await;

    tracing::info!("Declined invitation {}", invitation.id);
    HttpResponse::Ok().json(serde_json::json!({
        "message": "Invitation declined"
    }))
}

/// Best-effort: open a Noise client to the coordinator and send a
/// `DeclineInvitation` message. Logs (but does not propagate) failures —
/// callers treat this as fire-and-forget.
async fn notify_coordinator_of_decline(data: &web::Data<AppState>, invitation: &PendingInvitation) {
    let coordinator = match find_coordinator_peer(data, &invitation.coordinator_pubkey).await {
        Some(p) => p,
        None => {
            tracing::warn!(
                "Cannot notify coordinator of decline: no peer record for pubkey {}",
                invitation.coordinator_pubkey
            );
            return;
        }
    };

    let payload = DeclineInvitationPayload {
        kind: invitation.invitation_type.into(),
        reason: None,
        // Echo the coordinator's run identity so it only fails the matching
        // run — a stale card's decline must not kill a newer workflow.
        workflow_instance: invitation.workflow_instance.clone(),
    };
    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Failed to encode DeclineInvitationPayload: {e}");
            return;
        }
    };

    // Route the decline to the coordinator's matching run so it fails only that
    // run when multiple workflows are active.
    let route_instance = invitation.workflow_instance.clone().unwrap_or_default();
    let client = match NoiseClient::new(data.config.clone(), coordinator, route_instance).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to build NoiseClient for decline notification: {e}");
            return;
        }
    };

    // Bounded retries: this is the only signal that fails the coordinator's
    // run — its WaitingForPeers is human-paced (no timeout), so a single
    // dropped notification would leave that run hanging until a manual
    // cancel. Every other cross-node workflow call retries; so does this.
    let max_attempts = 3u32;
    for attempt in 1..=max_attempts {
        match client.send_decline_invitation(payload_bytes.clone()).await {
            Ok(()) => return,
            Err(e) if attempt < max_attempts => {
                tracing::warn!(
                    "Decline notification to coordinator failed \
                     (attempt {attempt}/{max_attempts}), retrying: {e}"
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => {
                tracing::warn!(
                    "Best-effort decline notification to coordinator failed after \
                     {max_attempts} attempts: {e}"
                );
            }
        }
    }
}

/// Look up the `Peer` record that owns the given Noise public key. The
/// invitation carries the coordinator's pubkey but `get_peer` is keyed by
/// participant id, so iterate the peers table.
async fn find_coordinator_peer(
    data: &web::Data<AppState>,
    coordinator_pubkey: &str,
) -> Option<Peer> {
    data.db
        .get_all_peers()
        .await
        .ok()?
        .into_iter()
        .find(|p| p.public_key == coordinator_pubkey)
}
