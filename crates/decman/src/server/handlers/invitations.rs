//! `GET /invitations`, `POST /invitations/accept`, `POST /invitations/decline`.
//!
//! A card is an active `WorkflowProposal` that names this node and that the
//! operator has not decided on. The observer projects the cards every tick
//! (`onledger::proposals::project_pending_invitations`); the handlers read
//! that projection and hand decisions to the engine (design D6, D10).

use std::collections::HashMap;

use actix_web::{HttpRequest, HttpResponse, Responder, get, http::StatusCode, post, web};

use crate::{
    db::schema::SchemaRead,
    onledger,
    server::{
        AppState,
        middleware::require_admin,
        types::{
            ErrorResponse, InvitationActionRequest, MessageResponse, PendingInvitation,
            PendingInvitationsResponse,
        },
    },
};

/// The HTTP status for an accept or decline error, from the words the engine
/// uses: an unknown or vanished proposal is 404, a proposal already decided,
/// expired, or not addressed to this node is 409, and a missing node identity
/// is 409 as well because the operator has to configure one.
fn decision_error_status(message: &str) -> StatusCode {
    let lower = message.to_ascii_lowercase();
    if lower.contains("not active") || lower.contains("not visible") {
        StatusCode::NOT_FOUND
    } else if lower.contains("already")
        || lower.contains("expired")
        || lower.contains("does not invite")
        || lower.contains("created by this node")
        || lower.contains("node identity")
    {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

fn decision_error(action: &str, e: &anyhow::Error) -> HttpResponse {
    let message = format!("{e:#}");
    let status = decision_error_status(&message);
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::error!(error = %message, "invitation {action} failed");
    } else {
        tracing::info!(error = %message, "invitation {action} refused");
    }
    HttpResponse::build(status).json(ErrorResponse {
        error: format!("Failed to {action} the invitation: {e}"),
    })
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
    let invitations = data.onledger.pending_invitations().await;

    // Resolve coordinator names from a single DB query
    let names: HashMap<String, String> = data
        .db
        .get_all_peers()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.participant_id.to_string(), p.name))
        .collect();

    let invitations_with_names: Vec<PendingInvitation> = invitations
        .into_iter()
        .map(|inv| PendingInvitation {
            coordinator_name: names.get(&inv.coordinator_participant).cloned(),
            ..inv
        })
        .collect();

    HttpResponse::Ok().json(PendingInvitationsResponse {
        invitations: invitations_with_names,
    })
}

/// Accept a pending invitation: record the decision and create the peer run
/// row. The observer accepts on the ledger and drives the run from there.
#[utoipa::path(
    tag = "Invitations",
    request_body = InvitationActionRequest,
    responses(
        (status = 200, description = "Invitation accepted", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "Invitation not found", body = ErrorResponse),
        (status = 409, description = "Invitation already decided, expired, or no node identity", body = ErrorResponse)
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
    match onledger::accept_invitation(&data.onledger, &body.id).await {
        Ok(accepted) => {
            tracing::info!(
                proposal = %body.id,
                instance = %accepted.instance_name,
                variant = ?accepted.member_variant,
                "invitation accepted"
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Invitation accepted".to_string(),
            })
        }
        Err(e) => decision_error("accept", &e),
    }
}

/// Decline a pending invitation: record the decision and exercise
/// `WorkflowProposal_Decline` so the coordinator's run fails at once.
#[utoipa::path(
    tag = "Invitations",
    request_body = InvitationActionRequest,
    responses(
        (status = 200, description = "Invitation declined", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "Invitation not found", body = ErrorResponse),
        (status = 409, description = "Invitation already accepted or no node identity", body = ErrorResponse)
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
    match onledger::decline_invitation(&data.onledger, &body.id, "declined by the operator").await {
        Ok(decline_cid) => {
            tracing::info!(proposal = %body.id, decline = %decline_cid, "invitation declined");
            HttpResponse::Ok().json(MessageResponse {
                message: "Invitation declined".to_string(),
            })
        }
        Err(e) => decision_error("decline", &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_errors_map_to_the_operator_facing_status() {
        assert_eq!(
            decision_error_status("WorkflowProposal 00a is not active or not visible to this node"),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            decision_error_status("WorkflowProposal 00a was already declined"),
            StatusCode::CONFLICT
        );
        assert_eq!(
            decision_error_status("WorkflowProposal 00a has expired"),
            StatusCode::CONFLICT
        );
        assert_eq!(
            decision_error_status("WorkflowProposal 00a does not invite this node"),
            StatusCode::CONFLICT
        );
        assert_eq!(
            decision_error_status("node identity not configured"),
            StatusCode::CONFLICT
        );
        assert_eq!(
            decision_error_status("database is locked"),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
