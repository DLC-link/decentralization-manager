//! Converting a local party to an externally signed one.

use actix_web::{HttpRequest, HttpResponse, Responder, post, web};
use base64::{Engine, engine::general_purpose::STANDARD};

use super::{
    audit::audited_write,
    wire::{add_hosts_error_response, decode_all, decode_public_key, serial_status_after},
};
use crate::{
    server::{
        AppState,
        middleware::tenant_caller,
        tenant_call_audit::TenantEndpoint,
        types::{
            ErrorResponse, LocalPartyAdoptExecuteRequest, LocalPartyAdoptRequest,
            TenantAddHostsExecuteResponse, TenantAddHostsPrepareResponse,
        },
    },
    workflow::external_party::{
        keys::fingerprint_from_public_key,
        local_party::{LocalPartyAdoptionPayload, prepare_adoption, submit_adoption},
    },
};

// ============================================================================
// Converting a local party (Plan B1)
// ============================================================================

/// Prepare a local party's conversion to an externally-signed one, returning the
/// hash the adopted key must sign.
///
/// Only the node whose namespace owns the party can serve this, and even it
/// cannot complete the conversion alone: Canton requires "party namespace + all
/// the new signing key", so the owner's signature over this hash is what proves
/// they hold the key the party will answer to.
#[utoipa::path(
    tag = "Tenant",
    request_body = LocalPartyAdoptRequest,
    responses(
        (status = 200, description = "Unsigned conversion", body = TenantAddHostsPrepareResponse),
        (status = 400, description = "Not local to this participant, already converted, or a bad key", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No authorized mapping for this party on this host", body = ErrorResponse),
        (status = 409, description = "The pinned base serial has moved on this host", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this host", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/local-party/adopt-key/prepare")]
pub async fn tenant_local_party_adopt_prepare(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<LocalPartyAdoptRequest>,
) -> impl Responder {
    if let Err(resp) = tenant_caller(&http_req) {
        return resp;
    }
    let public_key = match decode_public_key(&body.public_key) {
        Ok(key) => key,
        Err(resp) => return resp,
    };

    match prepare_adoption(&data.config, &body.party_id, &public_key, body.base_serial).await {
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
        Err(e) => add_hosts_error_response("local-party adopt prepare", e),
    }
}

/// Submit the owner-signed conversion. The node co-signs with its namespace key,
/// which for a local party is the party's own namespace.
#[utoipa::path(
    tag = "Tenant",
    request_body = LocalPartyAdoptExecuteRequest,
    responses(
        (status = 202, description = "Submitted on this host", body = TenantAddHostsExecuteResponse),
        (status = 400, description = "Bad request, not local to this participant, or a bundle that changes more than adopting the key", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No authorized mapping for this party on this host", body = ErrorResponse),
        (status = 409, description = "The pinned base serial has moved on this host", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this host", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/local-party/adopt-key/execute")]
pub async fn tenant_local_party_adopt_execute(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<LocalPartyAdoptExecuteRequest>,
) -> impl Responder {
    let key = match tenant_caller(&http_req) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let body = body.into_inner();
    let party_id = body.party_id.clone();
    audited_write(&data, key, TenantEndpoint::LocalPartyAdoptExecute, party_id, move |data| async move {
        tenant_local_party_adopt_execute_on_this_host(&data, &body).await
    })
    .await
}

async fn tenant_local_party_adopt_execute_on_this_host(
    data: &AppState,
    body: &LocalPartyAdoptExecuteRequest,
) -> HttpResponse {
    let public_key = match decode_public_key(&body.public_key) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    // The same check `tenant_execute` makes. Canton refuses the mismatch anyway,
    // but as a 500 reading "AddTransactions RPC failed" — which does not tell
    // the caller which of the two fields was wrong.
    let derived_fingerprint = fingerprint_from_public_key(&public_key);
    if derived_fingerprint != body.signed_by {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "signed_by ({signed_by}) does not match the fingerprint derived from public_key \
                 ({derived_fingerprint})",
                signed_by = body.signed_by
            ),
        });
    }
    let topology_transactions =
        match decode_all(&body.topology_transactions, "topology transaction") {
            Ok(v) => v,
            Err(error) => return HttpResponse::BadRequest().json(ErrorResponse { error }),
        };
    let signatures = match decode_all(&body.signatures, "signature") {
        Ok(v) => v,
        Err(error) => return HttpResponse::BadRequest().json(ErrorResponse { error }),
    };

    let bundle = LocalPartyAdoptionPayload {
        party_id: body.party_id.clone(),
        base_serial: body.base_serial,
        public_key,
        topology_transactions,
        signatures,
        signed_by: body.signed_by.clone(),
    };

    let base_serial = match submit_adoption(&data.config, &bundle).await {
        Ok(serial) => serial,
        Err(e) => return add_hosts_error_response("local-party adopt execute", e),
    };

    let (status, serial) = serial_status_after(
        &data.config,
        &body.party_id,
        base_serial,
        "local-party adopt",
    )
    .await;

    HttpResponse::Accepted().json(TenantAddHostsExecuteResponse {
        status,
        party_id: body.party_id.clone(),
        serial,
    })
}
