//! Parties this participant no longer hosts, and the purge of their leftover
//! contracts. The rules live in [`crate::server::former_parties`].

use actix_web::{HttpRequest, HttpResponse, Responder, delete, get, post, web};

use super::governance::get_party_token;
use crate::{
    canton_id::CantonId,
    db::schema::{SchemaRead, SchemaWrite},
    server::{
        AppState,
        audit::{AuditActor, PurgeAuditEvent, log_purge_audit},
        former_parties::{self, Refusal},
        middleware::require_admin,
        types::{
            ErrorResponse, FormerPartiesResponse, MessageResponse, PurgeFormerPartyRequest,
            PurgePreview, ReconnectFormerPartyRequest,
        },
    },
};

/// Which former party.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub struct FormerPartyQuery {
    pub party_id: String,
}

fn bad_party_id() -> HttpResponse {
    HttpResponse::BadRequest().json(ErrorResponse {
        error: "party_id must be a valid Canton id".to_string(),
    })
}

fn not_former(party_id: &CantonId) -> HttpResponse {
    HttpResponse::NotFound().json(ErrorResponse {
        error: format!("{party_id} is not a party this participant was removed from"),
    })
}

fn internal(context: &str, e: impl std::fmt::Display) -> HttpResponse {
    tracing::error!("{context}: {e}");
    HttpResponse::InternalServerError().json(ErrorResponse {
        error: format!("{context}: {e}"),
    })
}

/// List the parties this participant hosted once and no longer hosts.
///
/// A party appears here after a refresh of `/decentralized-parties` finds that
/// this participant left its hosting mapping. It disappears when the
/// participant hosts it again, or when an operator dismisses it after a purge.
#[utoipa::path(
    tag = "Parties",
    responses(
        (status = 200, description = "Former parties", body = FormerPartiesResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/former-parties")]
pub async fn list_former_parties(data: web::Data<AppState>) -> impl Responder {
    let rows = match data.db.get_former_dec_parties().await {
        Ok(rows) => rows,
        Err(e) => return internal("Failed to read the former parties", e),
    };
    let parties: Vec<_> = rows
        .into_iter()
        .filter_map(|row| match former_parties::to_former_party(row) {
            Ok(party) => Some(party),
            Err(e) => {
                tracing::warn!("Skipping a former party with an unreadable id: {e}");
                None
            }
        })
        .collect();
    HttpResponse::Ok().json(FormerPartiesResponse { parties })
}

/// Count what a purge of a former party would remove and keep right now.
#[utoipa::path(
    tag = "Parties",
    params(FormerPartyQuery),
    responses(
        (status = 200, description = "What the purge would do", body = PurgePreview),
        (status = 400, description = "Malformed party id", body = ErrorResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 404, description = "Not a former party of this participant", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/former-parties/purge-preview")]
pub async fn preview_former_party_purge(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    query: web::Query<FormerPartyQuery>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let Ok(party_id) = CantonId::parse(&query.party_id) else {
        return bad_party_id();
    };
    match data.db.get_former_dec_party(&party_id).await {
        Ok(Some(row)) if row.dismissed_at.is_none() => {}
        Ok(_) => return not_former(&party_id),
        Err(e) => return internal("Failed to read the former party", e),
    }
    let Some(token) = get_party_token(&data, &party_id).await else {
        return no_credentials(&party_id);
    };
    match former_parties::plan_purge(&data.config, &token, &party_id).await {
        Ok(split) => HttpResponse::Ok().json(PurgePreview {
            party_id,
            purge_count: split.purge.len(),
            keep_count: split.keep,
        }),
        Err(e) => internal("Failed to read the party's contracts", format!("{e:#}")),
    }
}

fn no_credentials(party_id: &CantonId) -> HttpResponse {
    HttpResponse::Conflict().json(ErrorResponse {
        error: format!(
            "This node has no ledger credentials for {party_id}, so it cannot read the \
             party's contracts"
        ),
    })
}

/// Purge a former party's leftover contracts from this participant.
///
/// Removes every contract of the party that no party this participant still
/// hosts is a stakeholder of. The participant leaves its synchronizer while the
/// purge runs, so every party it hosts is offline for that time. The purge
/// runs in the background; `GET /former-parties` reports its outcome.
#[utoipa::path(
    tag = "Parties",
    request_body = PurgeFormerPartyRequest,
    responses(
        (status = 202, description = "Purge started", body = MessageResponse),
        (status = 400, description = "Malformed party id", body = ErrorResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 404, description = "Not a former party of this participant", body = ErrorResponse),
        (status = 409, description = "A purge is unsafe right now; the error says why", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/former-parties/purge")]
pub async fn purge_former_party(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<PurgeFormerPartyRequest>,
) -> impl Responder {
    let actor = match require_admin(&http_req, data.admin_role.as_deref()) {
        Ok(principal) => AuditActor::from(&principal),
        Err(resp) => return resp,
    };
    let Ok(party_id) = CantonId::parse(&body.party_id) else {
        return bad_party_id();
    };

    match former_parties::check_purge_allowed(&data.config, &data.db, &party_id).await {
        Ok(Ok(())) => {}
        Ok(Err(Refusal::NotFormer)) => return not_former(&party_id),
        Ok(Err(Refusal::Unsafe(why))) => {
            tracing::warn!(%party_id, "Purge refused: {why}");
            return HttpResponse::Conflict().json(ErrorResponse { error: why });
        }
        Err(e) => return internal("Failed to check whether a purge is safe", format!("{e:#}")),
    }
    let Some(token) = get_party_token(&data, &party_id).await else {
        return no_credentials(&party_id);
    };
    match data.db.claim_former_party_purge(&party_id).await {
        Ok(true) => {}
        Ok(false) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "Another purge or a workflow started on this node. Try again after it \
                        finishes"
                    .to_string(),
            });
        }
        Err(e) => return internal("Failed to start the purge", e),
    }

    log_purge_audit(&data.db, PurgeAuditEvent::Start, &party_id, Some(&actor)).await;
    tokio::spawn(former_parties::run_purge(
        data.config.clone(),
        data.db.clone(),
        token,
        party_id.clone(),
        actor,
    ));
    HttpResponse::Accepted().json(MessageResponse {
        message: format!("Purge of {party_id} started"),
    })
}

/// Reconnect this participant to its synchronizer after a purge failed.
///
/// A purge reconnects the participant on every outcome. This covers the cases
/// it could not: decman stopped during the purge, or the reconnect failed.
/// Answers once the connection is healthy.
#[utoipa::path(
    tag = "Parties",
    request_body = ReconnectFormerPartyRequest,
    responses(
        (status = 200, description = "Connected and healthy again", body = MessageResponse),
        (status = 400, description = "Malformed party id", body = ErrorResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 404, description = "Not a former party of this participant", body = ErrorResponse),
        (status = 409, description = "A reconnect is unsafe right now; the error says why", body = ErrorResponse),
        (status = 500, description = "The reconnect failed", body = ErrorResponse)
    )
)]
#[post("/former-parties/reconnect")]
pub async fn reconnect_after_purge(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ReconnectFormerPartyRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let Ok(party_id) = CantonId::parse(&body.party_id) else {
        return bad_party_id();
    };
    match former_parties::reconnect_after_failed_purge(&data.config, &data.db, &party_id).await {
        Ok(Ok(())) => HttpResponse::Ok().json(MessageResponse {
            message: "The participant is connected and healthy again".to_string(),
        }),
        Ok(Err(Refusal::NotFormer)) => not_former(&party_id),
        Ok(Err(Refusal::Unsafe(why))) => {
            tracing::warn!(%party_id, "Reconnect refused: {why}");
            HttpResponse::Conflict().json(ErrorResponse { error: why })
        }
        Err(e) => internal("Failed to reconnect the participant", format!("{e:#}")),
    }
}

/// Hide a former party from the list once its leftovers are purged.
#[utoipa::path(
    tag = "Parties",
    params(FormerPartyQuery),
    responses(
        (status = 200, description = "Former party dismissed", body = MessageResponse),
        (status = 400, description = "Malformed party id", body = ErrorResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 404, description = "Not a former party of this participant", body = ErrorResponse),
        (status = 409, description = "The party's leftovers are not purged yet", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[delete("/former-parties")]
pub async fn dismiss_former_party(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    query: web::Query<FormerPartyQuery>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let Ok(party_id) = CantonId::parse(&query.party_id) else {
        return bad_party_id();
    };
    match data.db.dismiss_former_dec_party(&party_id).await {
        Ok(true) => HttpResponse::Ok().json(MessageResponse {
            message: format!("{party_id} dismissed"),
        }),
        Ok(false) => match data.db.get_former_dec_party(&party_id).await {
            Ok(Some(row)) if row.dismissed_at.is_none() => {
                HttpResponse::Conflict().json(ErrorResponse {
                    error: "Purge the party's leftover contracts before dismissing it".to_string(),
                })
            }
            Ok(_) => not_former(&party_id),
            Err(e) => internal("Failed to read the former party", e),
        },
        Err(e) => internal("Failed to dismiss the former party", e),
    }
}
