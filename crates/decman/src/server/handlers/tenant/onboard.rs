//! Wallet-driven onboarding: prepare, execute and status.

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    canton_id::validate_party_id_prefix,
    server::{
        AppState,
        middleware::tenant_caller,
        tenant_call_audit::TenantEndpoint,
        types::{
            ErrorResponse, TenantExecuteRequest, TenantExecuteResponse, TenantPrepareRequest,
            TenantPrepareResponse, WorkflowProgress, WorkflowStatusResponse,
        },
    },
    workflow::external_party::{
        keys::fingerprint_from_public_key,
        steps::{
            ExternalPartyAllocatePayload, HostOnboardingStatus, allocate_party,
            host_onboarding_status, prepare_topology,
        },
    },
};

use crate::server::handlers::workflows::validate_confirmation_threshold;

use super::{audit::audited_write, wire::decode_public_key};

// ============================================================================
// Onboarding (wallet-driven)
// ============================================================================

/// Prepare the onboarding topology for a wallet-held external party: DPM relays
/// the wallet's public key to Canton and returns the unsigned topology + the
/// per-transaction hashes for the wallet to sign.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantPrepareRequest,
    responses(
        (status = 200, description = "Unsigned onboarding topology", body = TenantPrepareResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/prepare")]
pub async fn tenant_prepare(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantPrepareRequest>,
) -> impl Responder {
    if let Err(resp) = tenant_caller(&http_req) {
        return resp;
    }
    if let Err(msg) = validate_party_id_prefix(&body.party_hint) {
        return HttpResponse::BadRequest().json(ErrorResponse { error: msg });
    }
    if body.hosting_peers.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "hosting_peers must name at least one other participant to host the party"
                .to_string(),
        });
    }
    let num_hosts = body.hosting_peers.len() + 1;
    if let Err(msg) = validate_confirmation_threshold(body.confirmation_threshold, num_hosts) {
        return HttpResponse::BadRequest().json(ErrorResponse { error: msg });
    }

    let public_key = match decode_public_key(&body.public_key) {
        Ok(pk) => pk,
        Err(resp) => return resp,
    };

    // No run is registered here — the wallet signs the returned hashes and calls
    // `/v0/tenant/execute` next.
    match prepare_topology(
        &data.config,
        &body.party_hint,
        &body.hosting_peers,
        body.confirmation_threshold,
        &public_key,
    )
    .await
    {
        Ok(prep) => HttpResponse::Ok().json(TenantPrepareResponse {
            party_id: prep.party_id,
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
        Err(e) => {
            tracing::error!("tenant prepare: topology generation failed: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to prepare the onboarding topology; see the host's logs".to_string(),
            })
        }
    }
}

/// Onboard a wallet-held external party from its signed topology on THIS host:
/// DPM allocates the wallet-signed bundle on its own participant only. The wallet
/// calls this on every host itself; no host relays to another. Idempotent
/// (`ALREADY_EXISTS` counts as success), so the wallet can safely retry a host.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantExecuteRequest,
    responses(
        (status = 202, description = "Allocated on this host (status reflects this host's view)", body = TenantExecuteResponse),
        (status = 400, description = "Bad request (bad base64, or signed_by != public_key fingerprint)", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "Allocation failed on this participant", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/execute")]
pub async fn tenant_execute(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantExecuteRequest>,
) -> impl Responder {
    let key = match tenant_caller(&http_req) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let body = body.into_inner();
    let party_id = format!("{}::{}", body.party_hint, body.signed_by);
    audited_write(
        &data,
        key,
        TenantEndpoint::Execute,
        party_id,
        move |data| async move { tenant_execute_on_this_host(&data, &body).await },
    )
    .await
}

async fn tenant_execute_on_this_host(data: &AppState, body: &TenantExecuteRequest) -> HttpResponse {
    if let Err(msg) = validate_party_id_prefix(&body.party_hint) {
        return HttpResponse::BadRequest().json(ErrorResponse { error: msg });
    }

    // The party id is built from the client-supplied `signed_by`, so it must be
    // the real fingerprint of `public_key`; otherwise a mismatched pair would
    // have us record and return a party id that doesn't match the key the
    // topology was signed with. Fail fast with a 400.
    let public_key = match decode_public_key(&body.public_key) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
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
    let mut signatures = Vec::with_capacity(body.signatures.len());
    for sig in &body.signatures {
        match STANDARD.decode(sig) {
            Ok(bytes) => signatures.push(bytes),
            Err(e) => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: format!("signature is not valid base64: {e}"),
                });
            }
        }
    }
    let mut topology_transactions = Vec::with_capacity(body.topology_transactions.len());
    for tx in &body.topology_transactions {
        match STANDARD.decode(tx) {
            Ok(bytes) => topology_transactions.push(bytes),
            Err(e) => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: format!("topology transaction is not valid base64: {e}"),
                });
            }
        }
    }

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

    let party_id = format!("{hint}::{fp}", hint = body.party_hint, fp = body.signed_by);
    let bundle = ExternalPartyAllocatePayload {
        party_id: party_id.clone(),
        public_key,
        topology_transactions,
        signatures,
        signed_by: body.signed_by.clone(),
    };

    // Submit on THIS participant only. The wallet calls `/execute` on every host
    // itself; no host relays to another. Canton keeps the topology a proposal until
    // every host has authorized it, and re-submitting an identical transaction is a
    // no-op, so a wallet retry converges.
    if let Err(e) = allocate_party(&data.config, &bundle).await {
        tracing::error!("tenant execute: allocate on this participant failed: {e:#}");
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: "Failed to allocate the external party on this host; see the host's logs"
                .to_string(),
        });
    }

    // Report this host's view: `Completed` once its authorized mapping names it,
    // else `InProgress` (still a proposal here — other hosts must sign).
    let status = match host_onboarding_status(&data.config, &party_id).await {
        Ok(HostOnboardingStatus::Hosted) => WorkflowProgress::Completed,
        Ok(_) => WorkflowProgress::InProgress,
        Err(e) => {
            tracing::warn!("tenant execute: post-allocate status read failed: {e:#}");
            WorkflowProgress::InProgress
        }
    };
    HttpResponse::Accepted().json(TenantExecuteResponse { status, party_id })
}

/// Onboarding status of a wallet-held party on THIS host, read from the
/// participant's topology. `party` must be the full party id (`{hint}::{fp}`).
/// `Completed` = this host's authorized `PartyToParticipant` names it;
/// `InProgress` = still a proposal here (this host has not finished signing);
/// 404 = no mapping at all. The wallet queries every host and aggregates.
#[utoipa::path(
    tag = "Tenant",
    params(("party" = String, Path, description = "Full party id")),
    responses(
        (status = 200, description = "Onboarding status on this host", body = WorkflowStatusResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No authorized PartyToParticipant for this party", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse)
    )
)]
#[get("/v0/tenant/{party}/status")]
pub async fn tenant_status(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = tenant_caller(&http_req) {
        return resp;
    }
    let party = path.into_inner();

    match host_onboarding_status(&data.config, &party).await {
        Ok(HostOnboardingStatus::Hosted) => HttpResponse::Ok().json(WorkflowStatusResponse {
            status: WorkflowProgress::Completed,
            error: None,
        }),
        Ok(HostOnboardingStatus::Pending) => HttpResponse::Ok().json(WorkflowStatusResponse {
            status: WorkflowProgress::InProgress,
            error: None,
        }),
        // Assigned here but still marked: the topology is authorized and the
        // party is not usable on this host yet. Reporting it Completed is what
        // this variant exists to stop.
        Ok(HostOnboardingStatus::Onboarding) => HttpResponse::Ok().json(WorkflowStatusResponse {
            status: WorkflowProgress::InProgress,
            error: Some(
                "hosted but still carrying Canton's onboarding marker, so the party is not \
                 usable here yet. Either the ACS has not been replicated or the clearing \
                 transaction is proposed and not yet authorized — the marker does not say \
                 which"
                    .to_string(),
            ),
        }),
        Ok(HostOnboardingStatus::Absent) => HttpResponse::NotFound().json(ErrorResponse {
            error: format!("This host does not host external party {party}"),
        }),
        Err(e) => {
            tracing::error!("tenant status: topology read failed: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to read the onboarding status; see the host's logs".to_string(),
            })
        }
    }
}
