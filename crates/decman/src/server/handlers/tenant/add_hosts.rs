//! Adding hosts to an existing party: prepare, authorize and execute.

use actix_web::{HttpRequest, HttpResponse, Responder, post, web};
use base64::{Engine, engine::general_purpose::STANDARD};

use super::{
    audit::audited_write,
    wire::{add_hosts_error_response, decode_all, serial_status_after},
};
use crate::{
    server::{
        AppState,
        middleware::tenant_caller,
        tenant_call_audit::TenantEndpoint,
        types::{
            ErrorResponse, TenantAddHostsExecuteRequest, TenantAddHostsExecuteResponse,
            TenantAddHostsPrepareResponse, TenantAddHostsRequest,
        },
    },
    workflow::external_party::add_hosts::{
        ExternalPartyAddHostsPayload, authorize_add_hosts, prepare_add_hosts, submit_add_hosts,
    },
};

// ============================================================================
// Add hosts to an existing party (wallet-driven)
// ============================================================================

/// Prepare the serial-N+1 topology that adds hosts to an existing external
/// party. Every host builds this independently and the wallet compares the
/// bytes, which is why `base_serial` is pinned: a host whose head state has
/// moved on refuses rather than returning different bytes from its peers.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantAddHostsRequest,
    responses(
        (status = 200, description = "Unsigned add-hosts topology", body = TenantAddHostsPrepareResponse),
        (status = 400, description = "Bad request, or a host set this party cannot take", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No authorized PartyToParticipant for this party", body = ErrorResponse),
        (status = 409, description = "This host reads a different serial for the party", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this host", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/add-hosts/prepare")]
pub async fn tenant_add_hosts_prepare(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantAddHostsRequest>,
) -> impl Responder {
    if let Err(resp) = tenant_caller(&http_req) {
        return resp;
    }
    if body.new_hosts.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "new_hosts must name at least one participant to add".to_string(),
        });
    }

    // `prepare_add_hosts` reads head state itself and reports *why* it refused,
    // so there is no pre-read here: a second read would only widen the window in
    // which the serial can move between the check and the build.
    // No ledger token: an external party's key is the wallet's, and this node
    // holds no credential for it. The offset capture degrades to its admin-API
    // tiers, which is exactly the tokenless path the tenant API is built on.
    match prepare_add_hosts(
        &data.config,
        &data.db,
        &body.party_id,
        &body.new_hosts,
        body.permission,
        body.base_serial,
        None,
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
        Err(e) => add_hosts_error_response("prepare", e),
    }
}

/// Authorize the add-hosts change with THIS node's own namespace key.
///
/// The counterpart to `add-hosts/execute` for a party nobody can sign for from
/// outside Canton: one that is, or once was, local. Its namespace is a
/// participant's root key, that key lives in Canton's vault, and adopting a
/// signing key does not move the namespace — so no wallet signature can ever
/// satisfy `execute`.
///
/// The caller invokes this on the node owning the party's namespace AND on each
/// joining node. Canton wants both halves for an add and neither node holds
/// both, so the change stays a proposal until the last call lands. Order does
/// not matter, and a repeat is harmless.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantAddHostsRequest,
    responses(
        (status = 202, description = "Authorized on this node", body = TenantAddHostsExecuteResponse),
        (status = 400, description = "Bad host set, or a change this node holds no key for", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No authorized PartyToParticipant for this party", body = ErrorResponse),
        (status = 409, description = "The pinned base serial has moved on this node", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this node", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/add-hosts/authorize")]
pub async fn tenant_add_hosts_authorize(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantAddHostsRequest>,
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
        TenantEndpoint::AddHostsAuthorize,
        party_id,
        move |data| async move { tenant_add_hosts_authorize_on_this_host(&data, &body).await },
    )
    .await
}

async fn tenant_add_hosts_authorize_on_this_host(
    data: &AppState,
    body: &TenantAddHostsRequest,
) -> HttpResponse {
    if body.new_hosts.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "new_hosts must name at least one participant to add".to_string(),
        });
    }

    // No ledger token, for the same reason prepare passes none: this node holds
    // no credential for the party, so the offset capture takes its admin-API
    // tiers.
    let base_serial = match authorize_add_hosts(
        &data.config,
        &data.db,
        &body.party_id,
        &body.new_hosts,
        body.permission,
        body.base_serial,
        None,
    )
    .await
    {
        Ok(serial) => serial,
        Err(e) => return add_hosts_error_response("authorize", e),
    };

    // The serial advances only once the last node has authorized; until then
    // this node's read still returns the base serial.
    let (status, serial) = serial_status_after(
        &data.config,
        &body.party_id,
        base_serial,
        "tenant add-hosts authorize",
    )
    .await;

    HttpResponse::Accepted().json(TenantAddHostsExecuteResponse {
        status,
        party_id: body.party_id.clone(),
        serial,
    })
}

/// Submit the wallet-signed add-hosts topology on THIS host. The wallet calls
/// this on every host that must authorize; no host relays to another.
/// Idempotent, so a wallet may safely retry a host.
///
/// The bundle is validated against this host's own head-state read before its
/// topology key co-signs anything — the party already exists and already holds
/// contracts, so a forged serial N+1 could otherwise evict its current hosts.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantAddHostsExecuteRequest,
    responses(
        (status = 202, description = "Submitted on this host", body = TenantAddHostsExecuteResponse),
        (status = 400, description = "Bad request, or topology that is not a plain add-hosts", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No authorized PartyToParticipant for this party", body = ErrorResponse),
        (status = 409, description = "The pinned base serial has moved on this host", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this host", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/add-hosts/execute")]
pub async fn tenant_add_hosts_execute(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantAddHostsExecuteRequest>,
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
        TenantEndpoint::AddHostsExecute,
        party_id,
        move |data| async move { tenant_add_hosts_execute_on_this_host(&data, &body).await },
    )
    .await
}

pub(super) async fn tenant_add_hosts_execute_on_this_host(
    data: &AppState,
    body: &TenantAddHostsExecuteRequest,
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
    if signatures.len() != topology_transactions.len() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "expected one signature per topology transaction, got {sigs} signature(s) for \
                 {txs} transaction(s)",
                sigs = signatures.len(),
                txs = topology_transactions.len()
            ),
        });
    }

    let bundle = ExternalPartyAddHostsPayload {
        party_id: body.party_id.clone(),
        base_serial: body.base_serial,
        topology_transactions,
        signatures,
        signed_by: body.signed_by.clone(),
    };

    let base_serial = match submit_add_hosts(&data.config, &bundle).await {
        Ok(serial) => serial,
        Err(e) => return add_hosts_error_response("execute", e),
    };

    // Report this host's view. The serial advances only once the change is
    // authorized; while it is still a proposal the read returns the base serial.
    let (status, serial) = serial_status_after(
        &data.config,
        &body.party_id,
        base_serial,
        "tenant add-hosts execute",
    )
    .await;

    HttpResponse::Accepted().json(TenantAddHostsExecuteResponse {
        status,
        party_id: body.party_id.clone(),
        serial,
    })
}

#[cfg(test)]
mod tests {
    use super::{super::test_support::test_participant, *};

    /// The wallet crate mirrors these types, so the wire names must not drift.
    /// Anything renamed here fails to deserialize on the other end.
    #[test]
    fn add_hosts_request_keeps_its_wire_shape() {
        let json = serde_json::json!({
            "party_id": "alice::1220aa",
            "new_hosts": [test_participant()],
            "base_serial": 4,
        });
        let Ok(request) = serde_json::from_value::<TenantAddHostsRequest>(json) else {
            panic!("the documented add-hosts request shape must deserialize");
        };
        assert_eq!(request.base_serial, 4);
        assert_eq!(request.new_hosts.len(), 1);
    }

    #[test]
    fn add_hosts_execute_request_keeps_its_wire_shape() {
        let json = serde_json::json!({
            "party_id": "alice::1220aa",
            "base_serial": 4,
            "topology_transactions": ["AQID"],
            "signatures": ["BAU="],
            "signed_by": "1220aa",
        });
        let Ok(request) = serde_json::from_value::<TenantAddHostsExecuteRequest>(json) else {
            panic!("the documented add-hosts execute shape must deserialize");
        };
        assert_eq!(
            request.signatures.len(),
            request.topology_transactions.len()
        );
        assert_eq!(request.signed_by, "1220aa");
    }
}
