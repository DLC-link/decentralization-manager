//! Confirmation threshold changes, party state and unhost.

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    server::{
        AppState,
        middleware::tenant_caller,
        tenant_call_audit::TenantEndpoint,
        types::{
            ErrorResponse, ExternalPartyUnhostResponse, TenantAddHostsExecuteResponse,
            TenantAddHostsPrepareResponse, TenantPartyStateResponse, TenantThresholdExecuteRequest,
            TenantThresholdRequest, TenantUnhostRequest, WorkflowProgress,
        },
    },
    workflow::external_party::{
        add_hosts::read_party_to_participant,
        threshold::{
            ExternalPartyThresholdPayload, authorize_threshold, prepare_threshold, submit_threshold,
        },
        unhost::UnhostRequest,
        unhost_audit::RequestedVia,
    },
};

use crate::server::handlers::unhost::run_unhost;

use super::{
    audit::{audited_write, tenant_requester},
    wire::{add_hosts_error_response, decode_all},
};

// ============================================================================
// Confirmation threshold
// ============================================================================

/// Prepare a threshold change. A separate serial bump from add-hosts, because a
/// host still carrying the onboarding marker cannot confirm and so cannot count
/// toward the new threshold: add, replicate, then raise.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantThresholdRequest,
    responses(
        (status = 200, description = "Unsigned threshold change", body = TenantAddHostsPrepareResponse),
        (status = 400, description = "A threshold this party cannot meet", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "This host does not host this party", body = ErrorResponse),
        (status = 409, description = "The pinned base serial has moved on this host", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this host", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/threshold/prepare")]
pub async fn tenant_threshold_prepare(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantThresholdRequest>,
) -> impl Responder {
    if let Err(resp) = tenant_caller(&http_req) {
        return resp;
    }
    match prepare_threshold(
        &data.config,
        &body.party_id,
        body.new_threshold,
        body.base_serial,
    )
    .await
    {
        Ok(prep) => HttpResponse::Ok().json(TenantAddHostsPrepareResponse {
            party_id: prep.party_id,
            serial: prep.serial,
            transaction_hashes: prep
                .transaction_hashes
                .iter()
                .map(|h| STANDARD.encode(h))
                .collect(),
            topology_transactions: prep
                .topology_transactions
                .iter()
                .map(|tx| STANDARD.encode(tx))
                .collect(),
        }),
        Err(e) => add_hosts_error_response("threshold prepare", e),
    }
}

/// Authorize a threshold change with THIS node's own namespace key.
///
/// The counterpart to `threshold/execute` for a party nobody can sign for from
/// outside Canton: one that is, or once was, local. `execute` submits the
/// caller's signature untouched and no node co-signs, so it can never serve a
/// party whose namespace key sits in Canton's vault.
///
/// Canton wants the party namespace alone for a threshold change, so this takes
/// one call on the node owning that namespace and the change is live when it
/// returns. A repeat is harmless.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantThresholdRequest,
    responses(
        (status = 202, description = "Authorized on this node", body = TenantAddHostsExecuteResponse),
        (status = 400, description = "A threshold this party cannot field, or a party this node holds no key for", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No authorized PartyToParticipant for this party", body = ErrorResponse),
        (status = 409, description = "The pinned base serial has moved on this node", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this node", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/threshold/authorize")]
pub async fn tenant_threshold_authorize(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantThresholdRequest>,
) -> impl Responder {
    let key = match tenant_caller(&http_req) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let body = body.into_inner();
    let party_id = body.party_id.clone();
    audited_write(
        &data,
        key,
        TenantEndpoint::ThresholdAuthorize,
        party_id,
        move |data| async move { tenant_threshold_authorize_on_this_host(&data, &body).await },
    )
    .await
}

async fn tenant_threshold_authorize_on_this_host(
    data: &AppState,
    body: &TenantThresholdRequest,
) -> HttpResponse {
    let base_serial = match authorize_threshold(
        &data.config,
        &body.party_id,
        body.new_threshold,
        body.base_serial,
    )
    .await
    {
        Ok(serial) => serial,
        Err(e) => return add_hosts_error_response("threshold authorize", e),
    };

    // One signature is all Canton wants here, so the change should be live
    // already; the read only reports what this node now sees.
    let (status, serial) = match read_party_to_participant(&data.config, &body.party_id).await {
        Ok(Some(current)) if current.serial > base_serial => {
            (WorkflowProgress::Completed, current.serial)
        }
        Ok(Some(current)) => (WorkflowProgress::InProgress, current.serial),
        Ok(None) => (WorkflowProgress::InProgress, base_serial),
        Err(e) => {
            tracing::warn!("tenant threshold authorize: post-submit status read failed: {e:#}");
            (WorkflowProgress::InProgress, base_serial)
        }
    };

    HttpResponse::Accepted().json(TenantAddHostsExecuteResponse {
        status,
        party_id: body.party_id.clone(),
        serial,
    })
}

/// Submit the wallet-signed threshold change on THIS host. A threshold change
/// needs the party namespace alone, so the party's signature is the complete
/// authorization and no host co-signs — but each host still validates what it
/// submits to its own store.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantThresholdExecuteRequest,
    responses(
        (status = 202, description = "Submitted on this host", body = TenantAddHostsExecuteResponse),
        (status = 400, description = "Bad request, or a bundle that changes more than the threshold", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "This host does not host this party", body = ErrorResponse),
        (status = 409, description = "The pinned base serial has moved on this host", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this host", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/threshold/execute")]
pub async fn tenant_threshold_execute(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantThresholdExecuteRequest>,
) -> impl Responder {
    let key = match tenant_caller(&http_req) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let body = body.into_inner();
    let party_id = body.party_id.clone();
    audited_write(
        &data,
        key,
        TenantEndpoint::ThresholdExecute,
        party_id,
        move |data| async move { tenant_threshold_execute_on_this_host(&data, &body).await },
    )
    .await
}

async fn tenant_threshold_execute_on_this_host(
    data: &AppState,
    body: &TenantThresholdExecuteRequest,
) -> HttpResponse {
    let topology_transactions =
        match decode_all(&body.topology_transactions, "topology transaction") {
            Ok(v) => v,
            Err(error) => return HttpResponse::BadRequest().json(ErrorResponse { error }),
        };
    let signatures = match decode_all(&body.signatures, "signature") {
        Ok(v) => v,
        Err(error) => return HttpResponse::BadRequest().json(ErrorResponse { error }),
    };

    let bundle = ExternalPartyThresholdPayload {
        party_id: body.party_id.clone(),
        base_serial: body.base_serial,
        topology_transactions,
        signatures,
        signed_by: body.signed_by.clone(),
    };

    let base_serial = match submit_threshold(&data.config, &bundle).await {
        Ok(serial) => serial,
        Err(e) => return add_hosts_error_response("threshold execute", e),
    };

    let (status, serial) = match read_party_to_participant(&data.config, &body.party_id).await {
        Ok(Some(current)) if current.serial > base_serial => {
            (WorkflowProgress::Completed, current.serial)
        }
        Ok(Some(current)) => (WorkflowProgress::InProgress, current.serial),
        Ok(None) => (WorkflowProgress::InProgress, base_serial),
        Err(e) => {
            tracing::warn!("tenant threshold execute: post-submit status read failed: {e:#}");
            (WorkflowProgress::InProgress, base_serial)
        }
    };

    HttpResponse::Accepted().json(TenantAddHostsExecuteResponse {
        status,
        party_id: body.party_id.clone(),
        serial,
    })
}

/// This host's view of a hosted party's topology, including the serial every
/// write in this API needs pinned.
///
/// Without it the writes are unusable by an actual wallet: they all require
/// `base_serial`, and a wallet has no Canton Admin API access and no other
/// endpoint that reports it. Reading it here is the first step of any change.
#[utoipa::path(
    tag = "Tenant",
    params(("party" = String, Path, description = "Full party id")),
    responses(
        (status = 200, description = "The party's current topology on this host", body = TenantPartyStateResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "This host holds no authorized mapping for this party", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this host", body = ErrorResponse)
    )
)]
#[get("/v0/tenant/{party}/state")]
pub async fn tenant_party_state(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = tenant_caller(&http_req) {
        return resp;
    }
    let party_id = path.into_inner();

    match read_party_to_participant(&data.config, &party_id).await {
        Ok(Some(current)) => {
            let onboarding_hosts = current
                .mapping
                .participants
                .iter()
                .filter(|p| p.onboarding.is_some())
                .count() as u32;
            HttpResponse::Ok().json(TenantPartyStateResponse {
                party_id,
                serial: current.serial,
                threshold: current.mapping.threshold,
                host_count: current.mapping.participants.len() as u32,
                onboarding_hosts,
                has_signing_key: current.mapping.party_signing_keys.is_some(),
            })
        }
        Ok(None) => HttpResponse::NotFound().json(ErrorResponse {
            error: format!("This host holds no authorized mapping for {party_id}"),
        }),
        Err(e) => {
            tracing::error!("tenant party state: topology read failed: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to read the party's topology; see the host's logs".to_string(),
            })
        }
    }
}

/// Make THIS host leave an external party, at the wallet provider's request.
///
/// The host removes only itself, with its own key, so no party signature is
/// involved. It archives the Splice contracts its validator holds for the party
/// first, then leaves the party's hosting mapping. A repeat after success
/// answers 200 with the current serial.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantUnhostRequest,
    responses(
        (status = 200, description = "This host no longer hosts the party", body = ExternalPartyUnhostResponse),
        (status = 202, description = "The topology write was accepted but is not effective yet; repeat the call", body = ExternalPartyUnhostResponse),
        (status = 400, description = "participant_id is not this host, this host is the last host, or the hosts left could not meet the threshold", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "This host does not host the party", body = ErrorResponse),
        (status = 409, description = "The pinned serial moved, or an unhost or ACS relay for the party is running", body = ErrorResponse),
        (status = 422, description = "This host has no validator operator party, and splice_cleanup was not false", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A step failed on this host", body = ExternalPartyUnhostResponse)
    )
)]
#[post("/v0/tenant/unhost")]
pub async fn tenant_unhost(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantUnhostRequest>,
) -> impl Responder {
    let key = match tenant_caller(&http_req) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let body = body.into_inner();
    run_unhost(
        &data,
        UnhostRequest {
            party_id: body.party_id,
            base_serial: body.base_serial,
            splice_cleanup: body.splice_cleanup.unwrap_or(true),
            requested_via: RequestedVia::Tenant,
            requested_by: tenant_requester(key.map(|k| k.index)),
            named_participant: Some(body.participant_id),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::super::test_support::test_participant;
    use super::*;

    #[test]
    fn unhost_request_keeps_its_wire_shape() {
        let json = serde_json::json!({
            "party_id": "alice::1220aa",
            "participant_id": test_participant(),
            "base_serial": 4,
        });
        let Ok(request) = serde_json::from_value::<TenantUnhostRequest>(json) else {
            panic!("the documented unhost request shape must deserialize");
        };
        assert_eq!(request.base_serial, 4);
        assert_eq!(
            request.splice_cleanup, None,
            "omitted means the cleanup runs"
        );
    }
}
