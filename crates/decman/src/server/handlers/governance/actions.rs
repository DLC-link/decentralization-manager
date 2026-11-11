//! Confirm, execute, expire and cancel endpoints.

use actix_web::{HttpRequest, HttpResponse, Responder, post, web};

use crate::server::{
    AppState,
    audit::{AuditEvent, AuditParams, spawn_audit_log},
    middleware::require_admin,
    scan::ScanNotConfigured,
    types::{
        CancelConfirmationRequest, CancelProposalRequest, ConfirmActionRequest, ErrorResponse,
        ExecuteActionRequest, ExpireConfirmationRequest, GovernanceType, MessageResponse,
    },
};

use super::{
    credentials::{get_party_credentials, packages},
    metrics::{ActionOutcome, ActionStep, count_action},
    propose::validate_inline_action,
    submit::{
        execute_cancel_confirmation, execute_cancel_proposal, execute_confirm_action,
        execute_confirmed_action, execute_expire_confirmation,
    },
};

/// Submit a confirmation for a governance action using structured ActionType
#[utoipa::path(
    tag = "Governance",
    request_body = ConfirmActionRequest,
    responses(
        (status = 200, description = "Confirmation submitted", body = MessageResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/confirm")]
pub async fn confirm_action(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ConfirmActionRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    if let Err(error) = validate_inline_action(&body.action, body.governance_type) {
        return HttpResponse::BadRequest().json(ErrorResponse { error });
    }

    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    let audit_pool = data.db.clone();
    let audit_summary = crate::server::audit::action_summary(&body.action);
    let audit_details = serde_json::to_string(&*body).unwrap_or_default();
    let audit_party_id = party_id.clone();
    let audit_member = member_party_id.clone();
    let audit_gov_type = body.governance_type;

    let packages = packages();

    let result =
        execute_confirm_action(&data.config, &body, &token, &member_party_id, &packages).await;
    count_action(ActionStep::Confirm, ActionOutcome::of(&result));
    match result {
        Ok(()) => {
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Confirm,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Confirmation submitted successfully".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Failed to submit confirmation: {e}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Confirm,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("{e}")),
                },
            );
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to submit confirmation: {e}"),
            })
        }
    }
}

/// Execute a confirmed governance action using structured ActionType
#[utoipa::path(
    tag = "Governance",
    request_body = ExecuteActionRequest,
    responses(
        (status = 200, description = "Action executed", body = MessageResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 422, description = "A paid credential accept needs DECPM_SCAN_URL, which is not set", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/execute")]
pub async fn execute_action(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ExecuteActionRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    if let Err(error) = validate_inline_action(&body.action, body.governance_type) {
        return HttpResponse::BadRequest().json(ErrorResponse { error });
    }

    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    let audit_pool = data.db.clone();
    let audit_summary = crate::server::audit::action_summary(&body.action);
    // Redact disclosed contract blobs (can be very large) before storing in audit
    let mut redacted = body.clone();
    for dc in &mut redacted.disclosed_contracts {
        dc.blob = format!("<{} bytes>", dc.blob.len());
    }
    let audit_details = serde_json::to_string(&redacted).unwrap_or_default();
    let audit_party_id = party_id.clone();
    let audit_member = member_party_id.clone();
    let audit_gov_type = body.governance_type;

    let packages = packages();

    let result = execute_confirmed_action(
        &data.config,
        &data.http_client,
        &body,
        &token,
        &member_party_id,
        &packages,
    )
    .await;
    count_action(ActionStep::Execute, ActionOutcome::of(&result));
    match result {
        Ok(()) => {
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Execute,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Action executed successfully".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Failed to execute action: {e:#}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Execute,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("{e:#}")),
                },
            );
            execute_failure_response(&e)
        }
    }
}

/// The execute handler's answer to a failed execute. A node without
/// `DECPM_SCAN_URL` cannot disclose what a paid credential accept reads. That
/// is a setting the caller can fix, so it is a 422, not a server fault. The
/// message carries the whole error chain, so a scan 403 reads differently from
/// a malformed reply or a dropped connection.
pub(super) fn execute_failure_response(e: &anyhow::Error) -> HttpResponse {
    let mut response = if e.downcast_ref::<ScanNotConfigured>().is_some() {
        HttpResponse::UnprocessableEntity()
    } else {
        HttpResponse::InternalServerError()
    };
    response.json(ErrorResponse {
        error: format!("Failed to execute action: {e:#}"),
    })
}

/// Expire a stale governance confirmation
#[utoipa::path(
    tag = "Governance",
    request_body = ExpireConfirmationRequest,
    responses(
        (status = 200, description = "Confirmation expired", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/expire")]
pub async fn expire_confirmation(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ExpireConfirmationRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    let audit_pool = data.db.clone();
    let audit_details = serde_json::to_string(&*body).unwrap_or_default();
    let audit_party_id = party_id.clone();
    let audit_member = member_party_id.clone();
    let audit_gov_type = body.governance_type;

    let packages = packages();

    let result =
        execute_expire_confirmation(&data.config, &body, &token, &member_party_id, &packages).await;
    count_action(ActionStep::Expire, ActionOutcome::of(&result));
    match result {
        Ok(()) => {
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Expire,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: "expire_confirmation".to_string(),
                    details: audit_details,
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Confirmation expired successfully".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Failed to expire confirmation: {e}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Expire,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: "expire_confirmation".to_string(),
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("{e}")),
                },
            );
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to expire confirmation: {e}"),
            })
        }
    }
}

/// Cancel (revoke) own governance confirmation
#[utoipa::path(
    tag = "Governance",
    request_body = CancelConfirmationRequest,
    responses(
        (status = 200, description = "Confirmation cancelled", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/cancel")]
pub async fn cancel_confirmation(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<CancelConfirmationRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let party_id = &body.party_id;

    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    let audit_pool = data.db.clone();
    let audit_details = serde_json::to_string(&*body).unwrap_or_default();
    let audit_party_id = party_id.clone();
    let audit_member = member_party_id.clone();
    let audit_gov_type = body.governance_type;

    let packages = packages();

    let result =
        execute_cancel_confirmation(&data.config, &body, &token, &member_party_id, &packages).await;
    count_action(ActionStep::CancelConfirmation, ActionOutcome::of(&result));
    match result {
        Ok(()) => {
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Cancel,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: "cancel_confirmation".to_string(),
                    details: audit_details,
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Confirmation cancelled successfully".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Failed to cancel confirmation: {e}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Cancel,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: audit_gov_type,
                    action_summary: "cancel_confirmation".to_string(),
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("{e}")),
                },
            );
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to cancel confirmation: {e}"),
            })
        }
    }
}

/// Retract a proposal the caller proposed
#[utoipa::path(
    tag = "Governance",
    request_body = CancelProposalRequest,
    responses(
        (status = 200, description = "Proposal cancelled", body = MessageResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/governance/cancel-proposal")]
pub async fn cancel_proposal(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<CancelProposalRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let party_id = &body.party_id;

    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: "No credentials configured for party".to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch auth token for party: {e}"),
            });
        }
    };

    let audit_pool = data.db.clone();
    let audit_details = serde_json::to_string(&*body).unwrap_or_default();
    let audit_party_id = party_id.clone();
    let audit_member = member_party_id.clone();

    let packages = packages();

    let result =
        execute_cancel_proposal(&data.config, &body, &token, &member_party_id, &packages).await;
    count_action(ActionStep::CancelProposal, ActionOutcome::of(&result));
    match result {
        Ok(()) => {
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::CancelProposal,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: GovernanceType::CoreDomain,
                    action_summary: "cancel_proposal".to_string(),
                    details: audit_details,
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Proposal cancelled successfully".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Failed to cancel proposal: {e}");
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::CancelProposal,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: GovernanceType::CoreDomain,
                    action_summary: "cancel_proposal".to_string(),
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("{e}")),
                },
            );
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to cancel proposal: {e}"),
            })
        }
    }
}

#[cfg(test)]
mod cancel_confirmation_gate_tests {
    use actix_web::{
        App,
        http::StatusCode,
        test::{self, TestRequest},
    };
    use serde_json::json;

    use super::*;
    use crate::server::middleware::AuthMiddleware;

    fn cancel_request() -> TestRequest {
        TestRequest::post()
            .uri("/governance/cancel")
            .set_json(json!({
                "party_id": format!("dec-party::{}", "0".repeat(68)),
                "confirmation_cid": "00confirmation",
            }))
    }

    /// The gate fires before the handler reads the member's stored credentials.
    #[actix_web::test]
    async fn a_request_without_a_principal_is_unauthorized() -> anyhow::Result<()> {
        let data = AppState::for_test(None).await?;
        let app = test::init_service(App::new().app_data(data).service(cancel_confirmation)).await;

        let resp = test::call_service(&app, cancel_request().to_request()).await;

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    /// A valid token without the admin role must not revoke a member's live
    /// confirmation as that member.
    #[actix_web::test]
    async fn a_non_admin_cannot_cancel_a_confirmation() -> anyhow::Result<()> {
        let data = AppState::for_test_as_non_admin().await?;
        let app = test::init_service(
            App::new()
                .app_data(data)
                .wrap(AuthMiddleware)
                .service(cancel_confirmation),
        )
        .await;

        let req = cancel_request()
            .insert_header(("Authorization", "Bearer spa.user.token"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        Ok(())
    }
}
