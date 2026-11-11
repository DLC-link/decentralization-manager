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
    credentials::{PartySession, packages},
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
    let PartySession {
        token,
        member_party_id,
    } = match PartySession::load(&data, party_id).await {
        Ok(session) => session,
        Err(e) => return e.into_response(),
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
    let PartySession {
        token,
        member_party_id,
    } = match PartySession::load(&data, party_id).await {
        Ok(session) => session,
        Err(e) => return e.into_response(),
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
    let PartySession {
        token,
        member_party_id,
    } = match PartySession::load(&data, party_id).await {
        Ok(session) => session,
        Err(e) => return e.into_response(),
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

    let PartySession {
        token,
        member_party_id,
    } = match PartySession::load(&data, party_id).await {
        Ok(session) => session,
        Err(e) => return e.into_response(),
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

    let PartySession {
        token,
        member_party_id,
    } = match PartySession::load(&data, party_id).await {
        Ok(session) => session,
        Err(e) => return e.into_response(),
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

#[cfg(test)]
mod audited_write_tests {
    //! Pins what each governance write answers and audits when its ledger
    //! call fails: the status code, the exact body, and the audit row.

    use std::{sync::Arc, time::Duration};

    use actix_web::{
        App, HttpMessage, body::to_bytes, dev::HttpServiceFactory, http::StatusCode,
        test as actix_test, web,
    };
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::json;
    use sqlx::SqlitePool;
    use tokio::sync::RwLock;

    use super::*;
    use crate::{
        auth::{MockAuthRegistry, Principal, WorkflowAuth},
        canton_id::CantonId,
        db::{MIGRATOR, rows::GovernanceAuditRow, schema::SchemaRead},
        server::handlers::governance::credentials::get_party_credentials,
    };

    type Result<T = ()> = anyhow::Result<T>;

    fn party() -> Result<CantonId> {
        CantonId::parse(&format!("p::{}", "0".repeat(68)))
    }

    fn closed_port() -> Result<u16> {
        Ok(std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port())
    }

    /// A node with mock credentials for every party, whose Canton ports have
    /// nothing listening on them.
    fn node(pool: SqlitePool) -> Result<web::Data<AppState>> {
        let auth = WorkflowAuth::Mock(Arc::new(MockAuthRegistry::new(Arc::new(RwLock::new(
            Vec::new(),
        )))));
        let mut state = AppState::for_test_on(Some(auth), pool).into_inner();
        let node = Arc::get_mut(&mut state)
            .ok_or_else(|| anyhow::anyhow!("a fresh test state has one owner"))?;
        node.config.canton.admin_api_port = closed_port()?;
        node.config.canton.ledger_api_port = closed_port()?;
        Ok(web::Data::from(state))
    }

    async fn post(
        data: &web::Data<AppState>,
        service: impl HttpServiceFactory + 'static,
        uri: &str,
        body: &serde_json::Value,
    ) -> Result<(StatusCode, Option<String>, String)> {
        let app =
            actix_test::init_service(App::new().app_data(data.clone()).service(service)).await;
        let request = actix_test::TestRequest::post()
            .uri(uri)
            .set_json(body)
            .to_request();
        request.extensions_mut().insert(Principal {
            sub: "admin".to_string(),
            issuer: "test".to_string(),
            roles: Vec::new(),
            email: None,
        });
        let response = actix_test::call_service(&app, request).await;
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = actix_test::read_body(response).await;
        Ok((status, content_type, String::from_utf8(body.to_vec())?))
    }

    /// The one audit row the write logged. The row is written by a spawned
    /// task, so wait for it.
    async fn the_audit_row(pool: &SqlitePool) -> Result<GovernanceAuditRow> {
        for _ in 0..100 {
            let mut rows = pool.get_governance_audit(&party()?, 50, 0).await?;
            if !rows.is_empty() {
                assert_eq!(rows.len(), 1, "{rows:?}");
                return rows.pop().ok_or_else(|| anyhow::anyhow!("no row"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        anyhow::bail!("no audit row was written")
    }

    /// The request as the handler reads it, and the token and member party
    /// it acts with.
    async fn request_of<T: DeserializeOwned>(
        data: &web::Data<AppState>,
        body: &serde_json::Value,
    ) -> Result<(T, String, CantonId)> {
        let (token, member) = get_party_credentials(data, &party()?)
            .await?
            .ok_or_else(|| anyhow::anyhow!("mock auth has credentials for every party"))?;
        Ok((serde_json::from_value(body.clone())?, token, member))
    }

    struct Expected<'a> {
        status: StatusCode,
        body: String,
        event_type: &'a str,
        member: &'a CantonId,
        governance_type: &'a str,
        action_summary: &'a str,
        details: String,
        error_message: String,
    }

    async fn assert_write(
        pool: &SqlitePool,
        answer: (StatusCode, Option<String>, String),
        expected: Expected<'_>,
    ) -> Result {
        let (status, content_type, body) = answer;
        assert_eq!(status, expected.status, "{body}");
        assert_eq!(content_type.as_deref(), Some("application/json"));
        assert_eq!(body, expected.body);
        let row = the_audit_row(pool).await?;
        assert_eq!(row.event_type, expected.event_type);
        assert_eq!(row.party_id, party()?.to_string());
        assert_eq!(row.member_party_id, expected.member.to_string());
        assert_eq!(row.governance_type, expected.governance_type);
        assert_eq!(row.action_summary, expected.action_summary);
        assert_eq!(row.details, expected.details);
        assert_eq!(row.status, "failed");
        assert_eq!(row.error_message, Some(expected.error_message));
        Ok(())
    }

    fn error_body(error: String) -> Result<String> {
        Ok(serde_json::to_string(&ErrorResponse { error })?)
    }

    fn details<T: Serialize>(request: &T) -> Result<String> {
        Ok(serde_json::to_string(request)?)
    }

    fn domain_action() -> serde_json::Value {
        json!({ "type": "governance_set_threshold", "new_threshold": 0 })
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_confirm_answers_and_audits(pool: SqlitePool) -> Result {
        let data = node(pool.clone())?;
        let body = json!({
            "party_id": party()?.to_string(),
            "rules_contract_id": "00rules",
            "action": domain_action(),
            "governance_type": "core_domain",
            "proposal_cid": "00proposal",
        });
        let (request, token, member): (ConfirmActionRequest, _, _) =
            request_of(&data, &body).await?;
        let error = execute_confirm_action(&data.config, &request, &token, &member, &packages())
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("the closed ledger must refuse the confirm"))?;

        let answer = post(&data, confirm_action, "/governance/confirm", &body).await?;

        assert_write(
            &pool,
            answer,
            Expected {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: error_body(format!("Failed to submit confirmation: {error}"))?,
                event_type: "confirm",
                member: &member,
                governance_type: "core_domain",
                action_summary: "governance_set_threshold",
                details: details(&request)?,
                error_message: format!("{error}"),
            },
        )
        .await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_execute_answers_and_audits(pool: SqlitePool) -> Result {
        let data = node(pool.clone())?;
        let body = json!({
            "party_id": party()?.to_string(),
            "rules_contract_id": "00rules",
            "action": domain_action(),
            "confirmation_cids": ["00confirmation"],
            "governance_type": "core_domain",
            "proposal_cid": "00proposal",
        });
        let (request, token, member): (ExecuteActionRequest, _, _) =
            request_of(&data, &body).await?;
        let error = execute_confirmed_action(
            &data.config,
            &data.http_client,
            &request,
            &token,
            &member,
            &packages(),
        )
        .await
        .err()
        .ok_or_else(|| anyhow::anyhow!("the closed ledger must refuse the execute"))?;
        let response = execute_failure_response(&error);
        let status = response.status();
        let expected_body = to_bytes(response.into_body())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let answer = post(&data, execute_action, "/governance/execute", &body).await?;

        assert_write(
            &pool,
            answer,
            Expected {
                status,
                body: String::from_utf8(expected_body.to_vec())?,
                event_type: "execute",
                member: &member,
                governance_type: "core_domain",
                action_summary: "governance_set_threshold",
                details: details(&request)?,
                error_message: format!("{error:#}"),
            },
        )
        .await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_expire_answers_and_audits(pool: SqlitePool) -> Result {
        let data = node(pool.clone())?;
        let body = json!({
            "party_id": party()?.to_string(),
            "rules_contract_id": "00rules",
            "confirmation_cid": "00confirmation",
        });
        let (request, token, member): (ExpireConfirmationRequest, _, _) =
            request_of(&data, &body).await?;
        let error =
            execute_expire_confirmation(&data.config, &request, &token, &member, &packages())
                .await
                .err()
                .ok_or_else(|| anyhow::anyhow!("the closed ledger must refuse the expire"))?;

        let answer = post(&data, expire_confirmation, "/governance/expire", &body).await?;

        assert_write(
            &pool,
            answer,
            Expected {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: error_body(format!("Failed to expire confirmation: {error}"))?,
                event_type: "expire",
                member: &member,
                governance_type: "core_self",
                action_summary: "expire_confirmation",
                details: details(&request)?,
                error_message: format!("{error}"),
            },
        )
        .await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_cancel_answers_and_audits(pool: SqlitePool) -> Result {
        let data = node(pool.clone())?;
        let body = json!({
            "party_id": party()?.to_string(),
            "confirmation_cid": "00confirmation",
        });
        let (request, token, member): (CancelConfirmationRequest, _, _) =
            request_of(&data, &body).await?;
        let error =
            execute_cancel_confirmation(&data.config, &request, &token, &member, &packages())
                .await
                .err()
                .ok_or_else(|| anyhow::anyhow!("the closed ledger must refuse the cancel"))?;

        let answer = post(&data, cancel_confirmation, "/governance/cancel", &body).await?;

        assert_write(
            &pool,
            answer,
            Expected {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: error_body(format!("Failed to cancel confirmation: {error}"))?,
                event_type: "cancel",
                member: &member,
                governance_type: "core_self",
                action_summary: "cancel_confirmation",
                details: details(&request)?,
                error_message: format!("{error}"),
            },
        )
        .await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_proposal_cancel_answers_and_audits(pool: SqlitePool) -> Result {
        let data = node(pool.clone())?;
        let body = json!({
            "party_id": party()?.to_string(),
            "proposal_cid": "00proposal",
        });
        let (request, token, member): (CancelProposalRequest, _, _) =
            request_of(&data, &body).await?;
        let error = execute_cancel_proposal(&data.config, &request, &token, &member, &packages())
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("the closed ledger must refuse the cancel"))?;

        let answer = post(&data, cancel_proposal, "/governance/cancel-proposal", &body).await?;

        assert_write(
            &pool,
            answer,
            Expected {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: error_body(format!("Failed to cancel proposal: {error}"))?,
                event_type: "cancel_proposal",
                member: &member,
                governance_type: "core_domain",
                action_summary: "cancel_proposal",
                details: details(&request)?,
                error_message: format!("{error}"),
            },
        )
        .await
    }
}
