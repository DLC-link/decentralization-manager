use actix_web::{HttpResponse, Responder, get, post, web};

use crate::server::{
    AppState,
    types::{
        InvitationActionRequest, InvitationType, PendingInvitation, PendingInvitationsResponse,
    },
};

/// Get all pending invitations
#[get("/invitations")]
pub async fn get_invitations(data: web::Data<AppState>) -> impl Responder {
    let invitations = data.pending_invitations.read().await;

    // Try to resolve coordinator names from network config
    let network_config = data.config.load_network_config().await.ok();
    let invitations_with_names: Vec<PendingInvitation> = invitations
        .iter()
        .map(|inv| {
            let coordinator_name = network_config.as_ref().and_then(|nc| {
                nc.peers
                    .iter()
                    .find(|p| p.public_key == inv.coordinator_pubkey)
                    .map(|p| p.name.clone())
            });
            PendingInvitation {
                coordinator_name,
                ..inv.clone()
            }
        })
        .collect();

    HttpResponse::Ok().json(PendingInvitationsResponse {
        invitations: invitations_with_names,
    })
}

/// Accept a pending invitation and trigger the workflow
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
        InvitationType::AddParty => {
            tracing::info!("Accepting add party invitation, triggering attestor workflow");
            data.add_party_trigger.notify_one();
        }
    }

    HttpResponse::Ok().json(serde_json::json!({
        "message": "Invitation accepted"
    }))
}

/// Decline a pending invitation
#[post("/invitations/decline")]
pub async fn decline_invitation(
    data: web::Data<AppState>,
    body: web::Json<InvitationActionRequest>,
) -> impl Responder {
    let mut invitations = data.pending_invitations.write().await;
    let idx = invitations.iter().position(|i| i.id == body.id);

    match idx {
        Some(i) => {
            invitations.remove(i);
            tracing::info!("Declined invitation {}", body.id);
            HttpResponse::Ok().json(serde_json::json!({
                "message": "Invitation declined"
            }))
        }
        None => HttpResponse::NotFound().json(serde_json::json!({
            "error": "Invitation not found"
        })),
    }
}
