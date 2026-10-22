//! Operator endpoints for making this node leave an external party, and the
//! response mapping the tenant endpoint shares with them.
//!
//! Both entry points run the same [`unhost`] workflow, so they answer with the
//! same status codes and the same body.

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use crate::{
    server::{
        AppState,
        middleware::require_admin,
        types::{
            ErrorResponse, ExternalPartyUnhostPreview, ExternalPartyUnhostRequest,
            ExternalPartyUnhostResponse, UnhostAuditResponse, WorkflowProgress,
        },
    },
    workflow::external_party::{
        unhost::{UnhostEnv, UnhostError, UnhostOutcome, UnhostRequest, preview, unhost},
        unhost_audit::{self, RequestedVia},
    },
};

/// How many attempts the audit endpoint returns.
const AUDIT_LIMIT: i64 = 100;

/// Make this node leave an external party.
///
/// The Splice contracts this node's validator holds for the party are archived
/// first, then the node removes itself from the party's hosting mapping with
/// its own key. Call the preview first: it reports the serial to pin and what
/// will be archived.
#[utoipa::path(
    tag = "Workflows",
    params(("party_id" = String, Path, description = "Full party id")),
    request_body = ExternalPartyUnhostRequest,
    responses(
        (status = 200, description = "This node no longer hosts the party", body = ExternalPartyUnhostResponse),
        (status = 202, description = "The topology write was accepted but is not effective yet; repeat the call", body = ExternalPartyUnhostResponse),
        (status = 400, description = "This node is the last host, or the hosts left could not meet the threshold", body = ErrorResponse),
        (status = 401, description = "Not authenticated", body = ErrorResponse),
        (status = 403, description = "Not an admin", body = ErrorResponse),
        (status = 404, description = "This node does not host the party", body = ErrorResponse),
        (status = 409, description = "The pinned serial moved, or an unhost or ACS relay for the party is running", body = ErrorResponse),
        (status = 422, description = "No validator operator party is set up, and splice_cleanup was not false", body = ErrorResponse),
        (status = 500, description = "A step failed; the audit row holds the reason", body = ExternalPartyUnhostResponse)
    )
)]
#[post("/external-parties/{party_id}/unhost")]
pub async fn unhost_external_party(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<ExternalPartyUnhostRequest>,
) -> impl Responder {
    let principal = match require_admin(&http_req, data.admin_role.as_deref()) {
        Ok(principal) => principal,
        Err(resp) => return resp,
    };
    let request = UnhostRequest {
        party_id: path.into_inner(),
        base_serial: body.base_serial,
        splice_cleanup: body.splice_cleanup.unwrap_or(true),
        requested_via: RequestedVia::Operator,
        requested_by: principal.email.unwrap_or(principal.sub),
        named_participant: None,
    };
    run_unhost(&data, request).await
}

/// What an unhost would do on this node right now. Nothing is written.
#[utoipa::path(
    tag = "Workflows",
    params(("party_id" = String, Path, description = "Full party id")),
    responses(
        (status = 200, description = "The serial to pin, the contracts to archive, and any blocker", body = ExternalPartyUnhostPreview),
        (status = 401, description = "Not authenticated", body = ErrorResponse),
        (status = 403, description = "Not an admin", body = ErrorResponse),
        (status = 404, description = "This node does not host the party", body = ErrorResponse),
        (status = 500, description = "A read failed on this node", body = ErrorResponse)
    )
)]
#[get("/external-parties/{party_id}/unhost/preview")]
pub async fn preview_unhost_external_party(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let party_id = path.into_inner();
    let auth = data.auth.read().await.clone();
    let env = UnhostEnv {
        config: &data.config,
        db: &data.db,
        relay_sessions: &data.relay_sessions,
        auth: auth.as_ref(),
    };
    match preview(&env, &party_id).await {
        Ok(preview) => HttpResponse::Ok().json(preview),
        Err(error) => {
            if let UnhostError::Internal(e) = &error {
                tracing::error!(%party_id, "external-party unhost preview failed: {e:#}");
            }
            unhost_error_response(error)
        }
    }
}

/// Every attempt to make this node leave the party, newest first.
#[utoipa::path(
    tag = "Workflows",
    params(("party_id" = String, Path, description = "Full party id")),
    responses(
        (status = 200, description = "The unhost attempts for the party", body = UnhostAuditResponse),
        (status = 500, description = "The audit could not be read", body = ErrorResponse)
    )
)]
#[get("/external-parties/{party_id}/unhost-audit")]
pub async fn get_unhost_audit(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let party_id = path.into_inner();
    match unhost_audit::list_for_party(&data.db, &party_id, AUDIT_LIMIT).await {
        Ok(rows) => HttpResponse::Ok().json(UnhostAuditResponse {
            entries: rows.into_iter().map(|row| row.into_entry()).collect(),
        }),
        Err(e) => {
            tracing::error!(%party_id, "failed to read the unhost audit: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to read the unhost audit; see the host's logs".to_string(),
            })
        }
    }
}

/// Run the workflow for either entry point and answer with its outcome.
///
/// The workflow runs in its own task. With a Splice cleanup on a busy network it
/// can outlast the caller: a wallet client gives up after 60 seconds, and actix
/// drops a handler whose client went away. Inside the handler that would stop
/// it between two steps; in its own task it finishes and closes its audit row.
pub(super) async fn run_unhost(data: &web::Data<AppState>, request: UnhostRequest) -> HttpResponse {
    let party_id = request.party_id.clone();
    let task = {
        let data = data.clone();
        tokio::spawn(async move {
            let auth = data.auth.read().await.clone();
            let env = UnhostEnv {
                config: &data.config,
                db: &data.db,
                relay_sessions: &data.relay_sessions,
                auth: auth.as_ref(),
            };
            let result = unhost(&env, &request).await;
            if matches!(&result, Ok(outcome) if outcome.status == WorkflowProgress::Completed) {
                data.external_parties.forget(&request.party_id);
            }
            result
        })
    };
    match task.await {
        Ok(Ok(outcome)) => unhost_outcome_response(&party_id, outcome),
        Ok(Err(error)) => unhost_error_response(error),
        Err(join) => {
            tracing::error!(%party_id, "the external-party unhost task did not finish: {join}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: "The unhost stopped unexpectedly; see the host's logs".to_string(),
            })
        }
    }
}

/// 200 once the node is gone, 202 while the write is not effective, and 500
/// with the same body when a step failed after work started.
fn unhost_outcome_response(party_id: &str, outcome: UnhostOutcome) -> HttpResponse {
    let mut builder = match outcome.status {
        WorkflowProgress::Completed => HttpResponse::Ok(),
        WorkflowProgress::InProgress => HttpResponse::Accepted(),
        _ => HttpResponse::InternalServerError(),
    };
    builder.json(ExternalPartyUnhostResponse {
        status: outcome.status,
        party_id: party_id.to_string(),
        serial: outcome.serial,
        validator_rights_archived: outcome.validator_rights_archived,
        preapprovals_cancelled: outcome.preapprovals_cancelled,
        audit_id: outcome.audit_id,
    })
}

/// Map a refusal onto the status code that names the remedy. The workflow has
/// already logged it.
pub(super) fn unhost_error_response(error: UnhostError) -> HttpResponse {
    let body = |error: &UnhostError| ErrorResponse {
        error: error.to_string(),
    };
    match &error {
        UnhostError::NotHosted { .. } => HttpResponse::NotFound().json(body(&error)),
        UnhostError::StaleSerial { .. } | UnhostError::Busy(_) => {
            HttpResponse::Conflict().json(body(&error))
        }
        UnhostError::Refused(_) => HttpResponse::BadRequest().json(body(&error)),
        UnhostError::NotConfigured(_) => HttpResponse::UnprocessableEntity().json(body(&error)),
        // The chain can name the admin endpoint, and this crosses the tenant
        // boundary. The audit row and the log hold the detail.
        UnhostError::Internal(_) => HttpResponse::InternalServerError().json(ErrorResponse {
            error: "A call failed on this host; see the host's logs".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;

    use super::*;

    /// Each refusal has its own remedy, so each keeps its own status code.
    #[test]
    fn refusals_map_to_distinct_status_codes() {
        let party = "alice::1220aa".to_string();
        let cases = [
            (
                UnhostError::NotHosted {
                    party: party.clone(),
                },
                StatusCode::NOT_FOUND,
            ),
            (
                UnhostError::StaleSerial {
                    party,
                    pinned: 4,
                    found: 5,
                },
                StatusCode::CONFLICT,
            ),
            (UnhostError::Busy("relay open".into()), StatusCode::CONFLICT),
            (
                UnhostError::Refused("last host".into()),
                StatusCode::BAD_REQUEST,
            ),
            (
                UnhostError::NotConfigured("no operator".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                UnhostError::Internal(anyhow::anyhow!("rpc failed")),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, expected) in cases {
            let label = error.to_string();
            assert_eq!(unhost_error_response(error).status(), expected, "{label}");
        }
    }

    fn outcome(status: WorkflowProgress) -> UnhostOutcome {
        UnhostOutcome {
            status,
            serial: 5,
            validator_rights_archived: vec!["vr".to_string()],
            preapprovals_cancelled: vec![],
            audit_id: 3,
        }
    }

    #[test]
    fn outcomes_map_to_200_202_and_500() {
        let party = "alice::1220aa";
        assert_eq!(
            unhost_outcome_response(party, outcome(WorkflowProgress::Completed)).status(),
            StatusCode::OK
        );
        assert_eq!(
            unhost_outcome_response(party, outcome(WorkflowProgress::InProgress)).status(),
            StatusCode::ACCEPTED
        );
        assert_eq!(
            unhost_outcome_response(party, outcome(WorkflowProgress::Failed)).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    /// The tenant contract the wallet crate mirrors. A renamed field fails to
    /// deserialize on the other end.
    #[test]
    fn the_unhost_response_keeps_its_wire_shape() {
        let json = serde_json::json!({
            "status": "completed",
            "party_id": "alice::1220aa",
            "serial": 5,
            "validator_rights_archived": ["vr"],
            "preapprovals_cancelled": [],
            "audit_id": 3,
        });
        let Ok(response) = serde_json::from_value::<ExternalPartyUnhostResponse>(json) else {
            panic!("the documented unhost response shape must deserialize");
        };
        assert_eq!(response.status, WorkflowProgress::Completed);
        assert_eq!(response.audit_id, 3);
    }
}
