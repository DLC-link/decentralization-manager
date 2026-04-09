use actix_web::{HttpResponse, Responder, get, post, web};

use std::collections::HashMap;

use crate::db::schema::SchemaRead;
use crate::server::{
    AppState,
    types::{
        ErrorResponse, InvitationActionRequest, InvitationType, MessageResponse, PendingInvitation,
        PendingInvitationsResponse,
    },
};

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
