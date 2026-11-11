//! Reward delegation and reward status endpoints.

use actix_web::{HttpResponse, Responder, get, web};

use super::credentials::{PartySession, packages};
use crate::server::{
    AppState,
    handlers::token_standard::GovernanceQuery,
    reward_automation::{active_delegations, reward_status},
    types::{ActiveCouponReassignmentDelegation, ErrorResponse, RewardStatusResponse},
};

/// Get the decparty's active CouponReassignmentDelegation contracts, newest first
///
/// The vote forms read this so nobody pastes a contract id: setup prefills
/// `prior_delegation`, revoke prefills the contract to archive. Normally zero or
/// one, but the singleton is not ledger-enforced, so several can appear.
#[utoipa::path(
    tag = "Governance",
    params(GovernanceQuery),
    responses(
        (
            status = 200,
            description = "Active delegations, newest first; empty when there are none",
            body = ActiveCouponReassignmentDelegation
        ),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Failed to fetch an auth token for party", body = ErrorResponse),
        (status = 503, description = "The delegations could not be read", body = ErrorResponse)
    )
)]
#[get("/coupon-reassignment-delegation")]
pub async fn get_coupon_reassignment_delegation(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let token = match PartySession::load(&data, party_id).await {
        Ok(session) => session.token,
        Err(e) => return e.into_response(),
    };

    match active_delegations(&data.config, &packages(), data.test_mode, party_id, &token).await {
        Ok(active) => HttpResponse::Ok().json(ActiveCouponReassignmentDelegation {
            delegations: active.into_iter().map(Into::into).collect(),
        }),
        // Never report a failed read as an empty list. The setup form would
        // prefill a blank `prior_delegation` and hit the propose-time 409 after
        // the whole split is filled in, and revoke would look unavailable.
        Err(e) => {
            tracing::warn!(%party_id, error = %e, "active delegation read failed");
            HttpResponse::ServiceUnavailable().json(ErrorResponse {
                error: format!("cannot read this party's current delegations: {e}"),
            })
        }
    }
}

/// Get the decparty's reward automation status on this node
///
/// Says whether this participant resolves the rewards DAR, which delegation is
/// active, and whether it lists this node's member party as an assigner. A
/// missing DAR is a 200 with `rewards_dar_resolvable: false`, not an error.
#[utoipa::path(
    tag = "Governance",
    params(GovernanceQuery),
    responses(
        (
            status = 200,
            description = "The reward status; `rewards_dar_resolvable` is false when this participant lacks the rewards DAR",
            body = RewardStatusResponse
        ),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 500, description = "Failed to fetch an auth token for party", body = ErrorResponse),
        (status = 503, description = "The reward status could not be read", body = ErrorResponse)
    )
)]
#[get("/reward-status")]
pub async fn get_reward_status(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let PartySession {
        token,
        member_party_id: member,
    } = match PartySession::load(&data, party_id).await {
        Ok(session) => session,
        Err(e) => return e.into_response(),
    };

    match reward_status(
        &data.config,
        &packages(),
        data.test_mode,
        party_id,
        &member,
        &token,
    )
    .await
    {
        Ok(status) => HttpResponse::Ok().json(status),
        Err(e) => {
            tracing::warn!(%party_id, error = %e, "reward status read failed");
            HttpResponse::ServiceUnavailable().json(ErrorResponse {
                error: format!("cannot read this party's reward status: {e}"),
            })
        }
    }
}
