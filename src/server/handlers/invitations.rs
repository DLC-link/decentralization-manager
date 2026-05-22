use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use sqlx::SqlitePool;

use crate::{
    config::Peer,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    noise::client::NoiseClient,
    server::{
        AppState,
        middleware::require_admin,
        types::{
            DeclineInvitationPayload, ErrorResponse, InvitationActionRequest, InvitationType,
            MessageResponse, PeerJob, PendingInvitation, PendingInvitationsResponse, WorkflowKind,
            WorkflowProgress, WorkflowRole, WorkflowRun,
        },
    },
    workflow::{ContractsStep, DarsStep, KickStep, OnboardingStep, state::WorkflowStep},
};

/// Best-effort: flip a peer-side `workflow_runs` row to Failed with a
/// caller-supplied reason. Used by `accept_invitation` when the peer-job
/// channel is closed so the feed doesn't keep a stuck InProgress row.
async fn mark_peer_run_failed(db: &SqlitePool, instance_name: &str, reason: &str) -> Result {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tx = db.begin_transaction().await?;
    tx.set_workflow_run_status(instance_name, WorkflowProgress::Failed, Some(reason), now)
        .await?;
    Commitable::commit(tx).await
}

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
/// The synthetic instance_name is `peer-<kind>-<coord_pubkey[..16]>-<ts>`
/// — only one accepted invite can be active per node at a time so the
/// timestamp suffix is enough to keep older completed rows distinct.
async fn insert_peer_run(
    data: &web::Data<AppState>,
    invitation: &PendingInvitation,
) -> Option<String> {
    let kind: WorkflowKind = invitation.invitation_type.into();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let pubkey_short =
        &invitation.coordinator_pubkey[..invitation.coordinator_pubkey.len().min(16)];
    let instance_name = format!(
        "peer-{}-{}-{}",
        kind.as_str().to_lowercase(),
        pubkey_short,
        now
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
        })
        .to_string(),
        coordinator_pubkey: Some(invitation.coordinator_pubkey.clone()),
        coordinator_name: None,
        // For onboarding the participants list is the authoritative peer
        // set. For other kinds we don't get a list from the invite.
        expected_peers: invitation.participants.clone(),
        completed_peers: Vec::new(),
        dec_party_id: None,
        prefix: None,
        participants: Vec::new(),
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
    // "I'm participating in <kind>" until completion. The instance_name we
    // mint here is the one we push onto the peer-job channel.
    let instance_name = match insert_peer_run(&data, &invitation).await {
        Some(name) => name,
        None => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to persist peer workflow run"
            }));
        }
    };

    let job = PeerJob {
        instance_name,
        coordinator_pubkey: invitation.coordinator_pubkey.clone(),
    };

    let sender = match invitation.invitation_type {
        InvitationType::Onboarding => &data.onboarding_peer_sender,
        InvitationType::Kick => &data.kick_peer_sender,
        InvitationType::Contracts => &data.contracts_peer_sender,
        InvitationType::Dars => &data.dars_peer_sender,
    };
    if let Err(send_err) = sender.send(job) {
        // Recover the instance_name we moved into the PeerJob and flip the
        // just-persisted peer row to Failed; otherwise the feed would show a
        // permanently-stuck "InProgress" run with no listener to finalize it.
        let stuck_instance = send_err.0.instance_name;
        tracing::error!(
            "Peer listener channel for {:?} closed; cannot dispatch invite for {stuck_instance}",
            invitation.invitation_type
        );
        if let Err(e) = mark_peer_run_failed(
            &data.db,
            &stuck_instance,
            "Peer listener channel closed before the job could be dispatched",
        )
        .await
        {
            tracing::warn!("Failed to mark stuck peer run {stuck_instance} as Failed: {e:#}");
        }
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "Peer listener unavailable; restart the server"
        }));
    }

    tracing::info!(
        "Accepted {:?} invitation, dispatched peer workflow job",
        invitation.invitation_type
    );

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
    };
    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Failed to encode DeclineInvitationPayload: {e}");
            return;
        }
    };

    let client = match NoiseClient::new(data.config.clone(), coordinator).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to build NoiseClient for decline notification: {e}");
            return;
        }
    };

    if let Err(e) = client.send_decline_invitation(payload_bytes).await {
        tracing::warn!("Best-effort decline notification to coordinator failed: {e}");
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
