use std::collections::HashMap;

use actix_web::{HttpResponse, Responder, get, post, web};

use crate::{
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    server::{
        AppState,
        types::{
            ErrorResponse, InvitationActionRequest, InvitationType, MessageResponse,
            PendingInvitation, PendingInvitationsResponse, WorkflowKind, WorkflowProgress,
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

/// Insert the attestor-side `workflow_runs` row for a freshly-accepted invite.
/// The synthetic instance_name is `attestor-<kind>-<coord_pubkey[..16]>-<ts>`
/// — only one accepted invite can be active per node at a time so the
/// timestamp suffix is enough to keep older completed rows distinct.
async fn insert_attestor_run(
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
    let instance_name = format!(
        "attestor-{}-{}-{}",
        kind.as_str().to_lowercase(),
        pubkey_short,
        now
    );
    let run = WorkflowRun {
        instance_name: instance_name.clone(),
        kind,
        role: WorkflowRole::Attestor,
        status: WorkflowProgress::InProgress,
        // No per-step granularity on the attestor side yet — coordinator drives
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
        // For onboarding the participants list is the authoritative attestor
        // set. For other kinds we don't get a list from the invite.
        expected_attestors: invitation.participants.clone(),
        completed_attestors: Vec::new(),
        dec_party_id: None,
        error: None,
        dismissed: false,
        created_at: now,
        updated_at: now,
    };

    let mut tx = match data.db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("attestor run: begin_transaction failed: {e}");
            return None;
        }
    };
    if let Err(e) = tx.upsert_workflow_run(&run).await {
        tracing::warn!("attestor run: upsert failed: {e}");
        return None;
    }
    if let Err(e) = Commitable::commit(tx).await {
        tracing::warn!("attestor run: commit failed: {e}");
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
        (status = 404, description = "Invitation not found", body = ErrorResponse)
    )
)]
#[post("/invitations/accept")]
pub async fn accept_invitation(
    data: web::Data<AppState>,
    body: web::Json<InvitationActionRequest>,
) -> impl Responder {
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

    // Persist an attestor-side workflow_runs row so the operator's feed shows
    // "I'm participating in <kind>" until completion. The trigger listener
    // reads `attestor_run_instance` to know which row to flip on terminal status.
    let attestor_instance = insert_attestor_run(&data, &invitation).await;
    {
        let mut slot = data.attestor_run_instance.write().await;
        *slot = attestor_instance;
    }

    // Store coordinator's public key and trigger the appropriate workflow
    {
        let mut coordinator_pubkey = data.coordinator_pubkey.write().await;
        *coordinator_pubkey = Some(invitation.coordinator_pubkey.clone());
    }

    match invitation.invitation_type {
        InvitationType::Onboarding => {
            tracing::info!("Accepting onboarding invitation, triggering attestor workflow");
            data.onboarding_trigger.notify_one();
        }
        InvitationType::Kick => {
            tracing::info!("Accepting kick invitation, triggering attestor workflow");
            data.kick_trigger.notify_one();
        }
        InvitationType::Contracts => {
            tracing::info!("Accepting contracts invitation, triggering attestor workflow");
            data.contracts_trigger.notify_one();
        }
        InvitationType::Dars => {
            tracing::info!("Accepting DARs invitation, triggering attestor workflow");
            data.dars_trigger.notify_one();
        }
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
        (status = 404, description = "Invitation not found", body = ErrorResponse)
    )
)]
#[post("/invitations/decline")]
pub async fn decline_invitation(
    data: web::Data<AppState>,
    body: web::Json<InvitationActionRequest>,
) -> impl Responder {
    let removed = {
        let mut invitations = data.pending_invitations.write().await;
        let idx = invitations.iter().position(|i| i.id == body.id);
        match idx {
            Some(i) => {
                invitations.remove(i);
                true
            }
            None => false,
        }
    };

    if !removed {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": "Invitation not found"
        }));
    }

    delete_persisted_invitation(&data, &body.id).await;
    tracing::info!("Declined invitation {}", body.id);
    HttpResponse::Ok().json(serde_json::json!({
        "message": "Invitation declined"
    }))
}
