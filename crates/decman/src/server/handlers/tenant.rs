//! Wallet-facing tenant API (`/v0/tenant/*`) — external-party onboarding.
//!
//! These endpoints let a wallet provider drive external-party onboarding for a
//! party whose Ed25519 namespace key the wallet generates and holds. DPM only
//! relays: it prepares the onboarding topology, and submits the wallet-signed
//! bundle to its own participant. Every binary field on the wire is base64
//! (STANDARD engine).
//!
//! Hosting the party is all this API manages. Transacting as the party — reading its
//! contracts, preparing and executing submissions, moving token-standard assets —
//! is the wallet's own job against Canton, because it needs a Ledger-API
//! credential the node has no business holding on the party's behalf. Onboarding,
//! by contrast, writes topology over the tokenless Admin API and needs no ledger
//! token at all. `unhost` is the one call that uses a ledger token, and it is the
//! node's own: its Splice cleanup acts as the node's validator operator party,
//! never as the external party.
//!
//! Unlike the admin endpoints (Keycloak JWT + `require_admin`), tenant callers
//! authenticate with a provider-issued API key. The auth middleware checks the
//! key before the body is read, and every handler here takes the matched key
//! from [`tenant_caller`].
//!
//! Each key may make `DECPM_TENANT_RATE_LIMIT_PER_MINUTE` calls per minute, and
//! a call past that gets a 429 with `Retry-After`. The ACS block read does not
//! count, because one copy sends a call per block. An ACS block import counts
//! when it opens a copy, or when no open copy takes it. The other blocks of an
//! open copy, the final one included, never count, so a copy is never refused
//! partway.

use std::panic::AssertUnwindSafe;

use actix_web::{HttpRequest, HttpResponse, Responder, body::MessageBody, get, post, web};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures::FutureExt;

use crate::{
    canton_id::validate_party_id_prefix,
    server::{
        AppState,
        middleware::{TenantKey, charge_tenant_key, tenant_caller},
        tenant_call_audit::{self, NewTenantCall, TenantEndpoint},
        types::{
            ErrorResponse, ExternalPartyUnhostResponse, LocalPartyAdoptExecuteRequest,
            LocalPartyAdoptRequest, TenantAcsBlockResponse, TenantAcsImportRequest,
            TenantAcsImportResponse, TenantAddHostsExecuteRequest, TenantAddHostsExecuteResponse,
            TenantAddHostsPrepareResponse, TenantAddHostsPublishRequest, TenantAddHostsRequest,
            TenantAddHostsStageResponse, TenantApproveHostingRequest, TenantExecuteRequest,
            TenantExecuteResponse, TenantPartyStateResponse, TenantPrepareRequest,
            TenantPrepareResponse, TenantThresholdExecuteRequest, TenantThresholdRequest,
            TenantUnhostRequest, WorkflowProgress, WorkflowStatusResponse,
        },
    },
    workflow::external_party::{
        add_hosts::{
            AddHostsError, ExternalPartyAddHostsPayload, authorize_add_hosts,
            import_onboarding_ready, import_window_owner, prepare_add_hosts, publish_add_hosts,
            read_party_to_participant, release_import_window_owner, replication_target,
            stage_add_hosts, submit_add_hosts,
        },
        decentralized_hosting::{Approval, ApproveHostingError, approve_hosting},
        keys::fingerprint_from_public_key,
        local_party::{LocalPartyAdoptionPayload, prepare_adoption, submit_adoption},
        steps::{
            ExternalPartyAllocatePayload, HostOnboardingStatus, allocate_party,
            host_onboarding_status, prepare_topology,
        },
        threshold::{
            ExternalPartyThresholdPayload, authorize_threshold, prepare_threshold, submit_threshold,
        },
        unhost::UnhostRequest,
        unhost_audit::RequestedVia,
    },
    workflow::party_replication::{
        ClearOutcome, ReplicationTarget, collect_party_package_ids, open_export_session,
        pipe::{PipeBlock, PipeTrailer},
        relay::{AcceptedBlock, ImportEnd, OnImportEnd, ServedBlock},
        request_onboarding_flag_clear, resume_tenant_import_window,
    },
};

#[cfg(test)]
use crate::db::MIGRATOR;
use crate::workflow::storage::artifact_kinds;

use super::{unhost::run_unhost, workflows::validate_confirmation_threshold};

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

// ============================================================================
// Hosting a decentralized party (covalidator approval)
// ============================================================================

/// Approve hosting a decentralized party on THIS host, as one of its
/// covalidators.
///
/// The owners' participants submit the party's onboarding topology first.
/// This host then finds the pending `PartyToParticipant` proposal in its own
/// synchronizer store by the hash the caller names, checks it, and signs it
/// with its own topology key. It signs nothing the caller sends. A party this
/// host already hosts answers `Completed`, so a retry is safe.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantApproveHostingRequest,
    responses(
        (status = 202, description = "Approved, or already hosted here (status reflects this host's view)", body = TenantExecuteResponse),
        (status = 400, description = "Bad request, or a proposal that fails a check", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No pending proposal for this party names this host yet; retry", body = ErrorResponse),
        (status = 409, description = "The pending proposal has another hash, or the party exists with other content", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this host", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/decentralized-party/approve-hosting")]
pub async fn tenant_approve_decentralized_hosting(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantApproveHostingRequest>,
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
        TenantEndpoint::DecentralizedPartyApproveHosting,
        party_id,
        move |data| async move { tenant_approve_hosting_on_this_host(&data, &body).await },
    )
    .await
}

async fn tenant_approve_hosting_on_this_host(
    data: &AppState,
    body: &TenantApproveHostingRequest,
) -> HttpResponse {
    let transaction_hash = match STANDARD.decode(&body.transaction_hash) {
        Ok(hash) => hash,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("transaction_hash is not valid base64: {e}"),
            });
        }
    };
    match approve_hosting(&data.config, &body.party_id, &transaction_hash).await {
        Ok(approval) => {
            let status = match approval {
                Approval::AlreadyHosted | Approval::Approved { hosted: true } => {
                    WorkflowProgress::Completed
                }
                Approval::Approved { hosted: false } => WorkflowProgress::InProgress,
            };
            HttpResponse::Accepted().json(TenantExecuteResponse {
                status,
                party_id: body.party_id.clone(),
            })
        }
        Err(error) => approve_hosting_error_response(error),
    }
}

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
        (status = 409, description = "The serial differs, or the joiner is busy, disconnected, or still relaying an ACS", body = ErrorResponse),
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

    let _admission = if body.new_hosts.contains(data.config.participant_id()) {
        let admission = data.tenant_import_mu.lock().await;
        if data.relay_sessions.any_session(|_| true).await {
            return add_hosts_error_response(
                "prepare",
                AddHostsError::Busy(
                    "an ACS relay is still open on this participant; finish it before preparing a joiner"
                        .into(),
                ),
            );
        }
        Some(admission)
    } else {
        None
    };

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
/// The namespace owner publishes the incomplete proposal here. Joining nodes
/// sign through `add-hosts/stage`, then a current host relays those signatures
/// through `add-hosts/publish`. Joining nodes cannot authorize through this route.
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
    let (status, serial) = match read_party_to_participant(&data.config, &body.party_id).await {
        Ok(Some(current)) if current.serial > base_serial => {
            (WorkflowProgress::Completed, current.serial)
        }
        Ok(Some(current)) => (WorkflowProgress::InProgress, current.serial),
        Ok(None) => (WorkflowProgress::InProgress, base_serial),
        Err(e) => {
            tracing::warn!("tenant add-hosts authorize: post-submit status read failed: {e:#}");
            (WorkflowProgress::InProgress, base_serial)
        }
    };

    HttpResponse::Accepted().json(TenantAddHostsExecuteResponse {
        status,
        party_id: body.party_id.clone(),
        serial,
    })
}

/// Publish the wallet-signed incomplete proposal on a current hosting participant.
/// Joining nodes must use `add-hosts/stage` before a current host publishes their signatures.
///
/// The bundle is validated against this host's own head-state read before
/// publication so a forged serial N+1 cannot change the existing hosts.
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

async fn tenant_add_hosts_execute_on_this_host(
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
    let (status, serial) = match read_party_to_participant(&data.config, &body.party_id).await {
        Ok(Some(current)) if current.serial > base_serial => {
            (WorkflowProgress::Completed, current.serial)
        }
        Ok(Some(current)) => (WorkflowProgress::InProgress, current.serial),
        Ok(None) => (WorkflowProgress::InProgress, base_serial),
        Err(e) => {
            tracing::warn!("tenant add-hosts execute: post-submit status read failed: {e:#}");
            (WorkflowProgress::InProgress, base_serial)
        }
    };

    HttpResponse::Accepted().json(TenantAddHostsExecuteResponse {
        status,
        party_id: body.party_id.clone(),
        serial,
    })
}

/// Sign the exact pending add-hosts proposal and disconnect before returning signatures.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantAddHostsRequest,
    responses(
        (status = 200, description = "Joiner is disconnected and its signatures are ready", body = TenantAddHostsStageResponse),
        (status = 400, description = "Invalid change or this participant is not a joiner", body = ErrorResponse),
        (status = 409, description = "Another import owns the participant or the serial moved", body = ErrorResponse),
        (status = 500, description = "A Canton call failed", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/add-hosts/stage")]
pub async fn tenant_add_hosts_stage(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantAddHostsRequest>,
) -> impl Responder {
    let key = match tenant_caller(&http_req) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let body = body.into_inner();
    let party = body.party_id.clone();
    audited_write(
        &data,
        key,
        TenantEndpoint::AddHostsStage,
        party,
        move |data| async move {
            let _admission = data.tenant_import_mu.lock().await;
            if data.relay_sessions.any_session(|_| true).await {
                return add_hosts_error_response(
                    "stage",
                    AddHostsError::Busy(
                        "an ACS relay is still open on this participant; finish it before staging"
                            .into(),
                    ),
                );
            }
            match stage_add_hosts(
                &data.config,
                &data.db,
                &body.party_id,
                &body.new_hosts,
                body.permission,
                body.base_serial,
            )
            .await
            {
                Ok(transactions) => HttpResponse::Ok().json(TenantAddHostsStageResponse {
                    party_id: body.party_id,
                    base_serial: body.base_serial,
                    signed_topology_transactions: transactions
                        .iter()
                        .map(|tx| STANDARD.encode(tx))
                        .collect(),
                }),
                Err(error) => add_hosts_error_response("stage", error),
            }
        },
    )
    .await
}

/// Publish disconnected joiners' signatures through a current hosting participant.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantAddHostsPublishRequest,
    responses(
        (status = 202, description = "Joiner signatures submitted", body = TenantAddHostsExecuteResponse),
        (status = 400, description = "The signatures do not describe the requested change", body = ErrorResponse),
        (status = 409, description = "The pinned serial moved", body = ErrorResponse),
        (status = 500, description = "A Canton call failed", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/add-hosts/publish")]
pub async fn tenant_add_hosts_publish(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantAddHostsPublishRequest>,
) -> impl Responder {
    let key = match tenant_caller(&http_req) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    let body = body.into_inner();
    let party = body.party_id.clone();
    audited_write(
        &data,
        key,
        TenantEndpoint::AddHostsPublish,
        party,
        move |data| async move {
            let transactions = match decode_all(
                &body.signed_topology_transactions,
                "signed topology transaction",
            ) {
                Ok(transactions) => transactions,
                Err(error) => return HttpResponse::BadRequest().json(ErrorResponse { error }),
            };
            match publish_add_hosts(
                &data.config,
                &body.party_id,
                &body.new_hosts,
                body.permission,
                body.base_serial,
                &transactions,
            )
            .await
            {
                Ok(base_serial) => {
                    let (status, serial) =
                        match read_party_to_participant(&data.config, &body.party_id).await {
                            Ok(Some(current)) if current.serial > base_serial => {
                                (WorkflowProgress::Completed, current.serial)
                            }
                            Ok(Some(current)) => (WorkflowProgress::InProgress, current.serial),
                            _ => (WorkflowProgress::InProgress, base_serial),
                        };
                    HttpResponse::Accepted().json(TenantAddHostsExecuteResponse {
                        party_id: body.party_id,
                        serial,
                        status,
                    })
                }
                Err(error) => add_hosts_error_response("publish", error),
            }
        },
    )
    .await
}

// ============================================================================
// ACS replication, relayed by the wallet
// ============================================================================

/// Serve one block of the party's ACS for `target`, for the wallet to relay.
///
/// Called on a host that already holds the party, AFTER the add-hosts topology
/// is authorized: Canton scopes the snapshot to the joiner's activation, which
/// must exist first. The offset it searches from was captured at prepare time,
/// before the topology moved.
///
/// Block 1 opens a fresh `ExportPartyAcs` stream and this host keeps it open
/// between requests, handing out successive blocks. Nothing is staged: one block
/// is in flight and neither end accumulates, which is what stops the size of the
/// party deciding whether the transfer is possible.
///
/// Forward-only, so there is no resume. The last block served is replayed if
/// asked for again — that makes a single transport retry safe — but anything
/// else is refused, and asking for block 1 again throws the stream away and
/// starts a new export.
#[utoipa::path(
    tag = "Tenant",
    params(
        ("party" = String, Path, description = "Full party id"),
        ("target" = String, Path, description = "Participant the snapshot is for"),
        ("base_serial" = u32, Query, description = "The serial the add-hosts write was pinned to"),
        ("seq" = u64, Query, description = "Block to serve, 1-based")
    ),
    responses(
        (status = 200, description = "One block of the ACS", body = TenantAcsBlockResponse),
        (status = 400, description = "Bad target participant id or seq", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 409, description = "Out of sync with this host's export; restart from block 1. Or this host is leaving the party", body = ErrorResponse),
        (status = 500, description = "Export failed on this host", body = ErrorResponse)
    )
)]
#[get("/v0/tenant/{party}/acs/{target}")]
pub async fn tenant_acs_snapshot(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<(String, String)>,
    query: web::Query<AcsBlockQuery>,
) -> impl Responder {
    // The middleware spends no budget here: one ACS copy asks for many blocks.
    if let Err(resp) = tenant_caller(&http_req) {
        return resp;
    }
    let (party_id, target) = path.into_inner();
    let target = match crate::canton_id::CantonId::parse(&target) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("target is not a valid participant id: {e}"),
            });
        }
    };
    let replication = match replication_target(&party_id, &target, query.base_serial) {
        Ok(t) => t,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("party_id is not a valid party id: {e}"),
            });
        }
    };
    if query.seq == 0 {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "seq is 1-based; block 0 does not exist".to_string(),
        });
    }

    let served = data
        .relay_sessions
        .serve_block(
            &data.config,
            &data.db,
            &replication,
            query.seq,
            EXPORT_BLOCK_SIZE,
            async || open_export_session(&data.config, &data.db, &replication).await,
        )
        .await;

    let block = match served {
        Ok(ServedBlock::Block(block)) => block,
        Ok(ServedBlock::OutOfSync { expected, asked }) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!(
                    "this host's export is at block {expected}, not {asked}; the export stream \
                     cannot rewind, so restart the transfer from block 1"
                ),
            });
        }
        Ok(ServedBlock::Held) => return relay_held_response(&party_id),
        Err(e) => {
            tracing::error!("tenant acs snapshot: export failed: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to export the party's ACS; see the host's logs".to_string(),
            });
        }
    };

    // No contracts means no packages to check, and the ledger scan is not free.
    // Only on block 1: the answer does not change mid-transfer, and the scan is
    // a ledger query that would otherwise run once per block.
    //
    // The scan is best-effort on purpose. It reads the party's contracts over
    // the Ledger API, which needs a credential for that party, and a node
    // hosting an *external* party has none — the key belongs to the wallet.
    // Failing the whole export over a preflight that cannot run on this
    // deployment would block replication entirely; instead the response says
    // the preflight is unavailable and the joiner's own import still validates
    // every contract, just after it has disconnected rather than before.
    let (package_ids, package_preflight) = if query.seq != 1 {
        (Vec::new(), true)
    } else {
        match collect_party_package_ids(&data.config, &party_id, None).await {
            Ok(ids) => (ids, true),
            Err(e) => {
                tracing::warn!(
                    "tenant acs snapshot: package preflight unavailable for {party_id} — this \
                     node holds no ledger credential for an external party, so the joiner's \
                     import will validate packages itself after disconnecting: {e:#}"
                );
                (Vec::new(), false)
            }
        }
    };

    let response = match block {
        PipeBlock::Data { seq, bytes } => TenantAcsBlockResponse {
            party_id,
            seq,
            chunk: STANDARD.encode(&bytes),
            end: false,
            total_len: 0,
            sha256: String::new(),
            package_ids,
            package_preflight,
        },
        PipeBlock::End { seq, trailer } => TenantAcsBlockResponse {
            party_id,
            seq,
            chunk: String::new(),
            end: true,
            total_len: trailer.total_len,
            sha256: trailer.sha256,
            package_ids,
            package_preflight,
        },
    };
    HttpResponse::Ok().json(response)
}

/// Take one relayed ACS block on THIS host, importing it straight into Canton.
///
/// The wallet hands over block N; this host feeds it into the `ImportPartyAcs`
/// stream it holds open for the whole transfer, and the call returns once Canton
/// has taken it. Nothing is buffered or staged: the response is the backpressure
/// that paces the wallet, and the size of the party never becomes a disk or
/// memory commitment here.
///
/// Block 1 starts a fresh import. There is no resume — the source's export
/// stream cannot rewind — so a broken transfer restarts from block 1 rather than
/// continuing, exactly as the add-party path does.
///
/// The final block finishes the import and clears the onboarding marker in the
/// same call. The two stay together: a host that imported but stayed marked is
/// not usable, and one that cleared without importing would start confirming
/// transactions it cannot validate.
///
/// **This host is off the synchronizer for the whole transfer.** `ImportPartyAcs`
/// requires it, and the window is now paced by the wallet. A session that goes
/// quiet is reaped, which fails the import and reconnects the participant.
#[utoipa::path(
    tag = "Tenant",
    request_body = TenantAcsImportRequest,
    responses(
        (status = 200, description = "Block taken; complete says whether the import finished", body = TenantAcsImportResponse),
        (status = 400, description = "Bad base64, bad party id, or missing packages", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "No authorized mapping for this party on this host", body = ErrorResponse),
        (status = 409, description = "Out of sync with this host's import; restart from block 1. Or this host is leaving the party", body = ErrorResponse),
        (status = 429, description = "Rate limit for this tenant API key exceeded; only a block that opens a copy, or that no open copy takes, counts. Retry-After gives the seconds to wait", body = ErrorResponse),
        (status = 500, description = "Import failed on this participant", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/add-hosts/import")]
pub async fn tenant_acs_import(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantAcsImportRequest>,
) -> impl Responder {
    // The middleware spends no budget here: one ACS copy sends many blocks.
    let key = match tenant_caller(&http_req) {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    // A copy this host admitted must never be refused partway, or the
    // staged participant must stay offline until its ACS is complete.
    // The next block of an open copy, the final one included, spends no budget.
    // Finalization runs in an audited task so cancellation cannot release its
    // admission lock while Canton is still accepting the snapshot.
    // Every other block spends budget: block 1, which opens a copy, block 0,
    // which does not exist, a block that fails the checks below, and a block
    // that no open copy takes next. Two requests that carry the same next
    // block both pass, and the import takes one: a wallet that retries a block
    // it saw time out must not get a 429.
    //
    // Block 0 and block 1 can never skip the budget, so they spend it before
    // the checks: a key with no budget left gets its 429 before the node
    // decodes the chunk.
    let opens = body.seq <= 1;
    if opens && let Err(resp) = charge_tenant_key(&data, key.as_ref(), http_req.path()) {
        return resp;
    }
    let checked = check_import_block(&data, &body);
    if !opens {
        let next = match &checked {
            Ok(checked) => {
                data.relay_sessions
                    .is_next_block(&checked.replication.instance_name, body.seq)
                    .await
            }
            Err(_) => false,
        };
        if next && !body.end {
            return tenant_acs_import_on_this_host(&data, &body, checked, None).await;
        }
        if !next && let Err(resp) = charge_tenant_key(&data, key.as_ref(), http_req.path()) {
            return resp;
        }
    }
    let endpoint = if opens {
        TenantEndpoint::AcsImportStart
    } else if body.end {
        TenantEndpoint::AcsImportEnd
    } else {
        return tenant_acs_import_on_this_host(&data, &body, checked, None).await;
    };
    let on_end = (body.seq == 1).then(|| record_import_end(&data, key.clone(), &body.party_id));
    let body = body.into_inner();
    let party_id = body.party_id.clone();
    audited_write(&data, key, endpoint, party_id, move |data| async move {
        tenant_acs_import_on_this_host(&data, &body, checked, on_end).await
    })
    .await
}

/// A relayed block that passed the checks that need no Canton call.
struct CheckedBlock {
    replication: crate::workflow::party_replication::ReplicationTarget,
    block: PipeBlock,
}

/// Check a relayed block without a Canton call, and decode its chunk once.
/// The error is the message of the 400 that refuses the block.
fn check_import_block(
    data: &AppState,
    body: &TenantAcsImportRequest,
) -> std::result::Result<CheckedBlock, String> {
    if body.seq == 0 {
        return Err("seq is 1-based; block 0 does not exist".to_string());
    }
    let chunk = STANDARD
        .decode(&body.chunk)
        .map_err(|e| format!("chunk is not valid base64: {e}"))?;
    let replication = replication_target(
        &body.party_id,
        data.config.participant_id(),
        body.base_serial,
    )
    .map_err(|e| format!("party_id is not a valid party id: {e}"))?;
    let block = if body.end {
        PipeBlock::End {
            seq: body.seq,
            trailer: PipeTrailer {
                total_len: body.total_len,
                sha256: body.sha256.clone(),
            },
        }
    } else {
        PipeBlock::Data {
            seq: body.seq,
            bytes: chunk,
        }
    };
    Ok(CheckedBlock { replication, block })
}

async fn tenant_acs_import_on_this_host(
    data: &AppState,
    body: &TenantAcsImportRequest,
    checked: std::result::Result<CheckedBlock, String>,
    on_end: Option<OnImportEnd>,
) -> HttpResponse {
    let CheckedBlock { replication, block } = match checked {
        Ok(checked) => checked,
        Err(error) => return HttpResponse::BadRequest().json(ErrorResponse { error }),
    };

    let _admission = data.tenant_import_mu.lock().await;
    let owner = match import_window_owner(&data.db, data.config.participant_id()).await {
        Ok(owner) => owner,
        Err(error) => {
            tracing::error!("tenant acs import: could not read import ownership: {error:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to read the import window; see the host's logs".into(),
            });
        }
    };
    if owner.as_deref() != Some(replication.instance_name.as_str()) {
        if owner.is_none() && body.end {
            match replication
                .read_artifact(
                    &data.db,
                    artifact_kinds::TENANT_ADD_HOSTS_IMPORT_COMPLETED,
                    None,
                )
                .await
            {
                Ok(Some(_)) => return completed_tenant_import(data, body, &replication).await,
                Ok(None) => {}
                Err(error) => {
                    tracing::error!("tenant acs import: completed-import lookup failed: {error:#}");
                    return HttpResponse::InternalServerError().json(ErrorResponse {
                        error: "Failed to read the completed import; see the host's logs".into(),
                    });
                }
            }
        }
        return HttpResponse::Conflict().json(ErrorResponse {
            error: "this replication does not own the participant's import window; stage the joiner before publishing hosting".into(),
        });
    }
    let imported = match replication
        .read_artifact(
            &data.db,
            artifact_kinds::TENANT_ADD_HOSTS_ACS_IMPORTED,
            None,
        )
        .await
    {
        Ok(imported) => imported.is_some(),
        Err(error) => return tenant_import_failure_response(data, &replication, error).await,
    };
    if imported {
        if !body.end {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "The ACS is already imported; retry the identical final block to finish reconnecting without importing again".into(),
            });
        }
        if data
            .relay_sessions
            .any_session(|name| name == replication.instance_name)
            .await
        {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "The imported ACS is still finishing its reconnect; retry the identical final block after that request finishes".into(),
            });
        }
        if let Err(error) = async {
            anyhow::ensure!(
                resume_tenant_import_window(&data.config, &data.db, &replication).await?,
                "the imported ACS proof is missing"
            );
            release_import_window_owner(&data.db, &replication).await
        }
        .await
        {
            return tenant_import_failure_response(data, &replication, error).await;
        }
        return completed_tenant_import(data, body, &replication).await;
    }
    if body.seq == 1 {
        match import_onboarding_ready(&data.config, &data.db, &replication, body.base_serial).await
        {
            Ok(true) => {}
            Ok(false) => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: "the exact staged onboarding proposal is not ready on this participant"
                        .into(),
                });
            }
            Err(error) => {
                tracing::error!("tenant acs import: staged topology check failed: {error:#}");
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: "Failed to check the staged topology; see the host's logs".into(),
                });
            }
        }
    }
    let db = data.db.clone();
    let completed_target = replication.clone();
    let on_end: Option<OnImportEnd> = Some(Box::new(move |outcome| {
        Box::pin(async move {
            if matches!(&outcome, ImportEnd::Completed)
                && let Err(error) = release_import_window_owner(&db, &completed_target).await
            {
                tracing::error!(
                    "tenant acs import: could not release completed import window: {error:#}"
                );
            }
            if let Some(previous) = on_end {
                previous(outcome).await;
            }
        })
    }));

    let accepted = data
        .relay_sessions
        .accept_block(
            &data.config,
            &data.db,
            &replication,
            &body.package_ids,
            block,
            on_end,
        )
        .await;

    match accepted {
        Ok(AcceptedBlock::Accepted { seq }) => HttpResponse::Ok().json(TenantAcsImportResponse {
            party_id: body.party_id.clone(),
            seq,
            complete: false,
            imported: false,
            marker_cleared: false,
        }),
        Ok(AcceptedBlock::Held) => relay_held_response(&body.party_id),
        Ok(AcceptedBlock::OutOfSync { expected, asked }) => {
            HttpResponse::Conflict().json(ErrorResponse {
                error: match expected {
                    Some(expected) => format!(
                        "this host is waiting for block {expected}, not {asked}; the import \
                         stream cannot rewind, so restart the transfer from block 1"
                    ),
                    None => format!(
                        "no ACS import is open on this host for {party}, so block {asked} has \
                         nowhere to go; restart the transfer from block 1",
                        party = body.party_id
                    ),
                },
            })
        }
        Ok(AcceptedBlock::Complete) => completed_tenant_import(data, body, &replication).await,
        Err(error) => tenant_import_failure_response(data, &replication, error).await,
    }
}

async fn tenant_import_failure_response(
    data: &AppState,
    target: &ReplicationTarget,
    error: anyhow::Error,
) -> HttpResponse {
    tracing::error!("tenant acs import: import or reconnect failed: {error:#}");
    let message = match target
        .read_artifact(
            &data.db,
            artifact_kinds::TENANT_ADD_HOSTS_ACS_IMPORTED,
            None,
        )
        .await
    {
        Ok(Some(_)) => {
            "The ACS is imported, but reconnect or health verification failed. Retry the identical final block without staging or importing again. See the host's logs"
        }
        Ok(None) => {
            "The ACS import failed on this host; the staged participant stays disconnected. Complete this import or repair it before reconnecting. See the host's logs"
        }
        Err(read_error) => {
            tracing::error!("tenant acs import: could not read imported ACS proof: {read_error:#}");
            "The import or reconnect failed and its durable status could not be read. Ask the operator to inspect this participant before retrying. See the host's logs"
        }
    };
    HttpResponse::InternalServerError().json(ErrorResponse {
        error: message.into(),
    })
}

async fn completed_tenant_import(
    data: &AppState,
    body: &TenantAcsImportRequest,
    target: &ReplicationTarget,
) -> HttpResponse {
    if let Err(error) = release_import_window_owner(&data.db, target).await {
        tracing::error!(
            "tenant acs import: could not persist completed import ownership: {error:#}"
        );
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: "The ACS is imported, but its durable import window could not be released. Retry the identical final block without staging or importing again. See the host's logs".into(),
        });
    }
    let marker_cleared = match request_onboarding_flag_clear(&data.config, &data.db, target).await {
        Ok(ClearOutcome::Cleared) => true,
        Ok(ClearOutcome::Proposed) => false,
        Err(error) => {
            tracing::error!("tenant acs import: requesting the marker clear failed: {error:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Imported the ACS, but could not request the marker clear; retry this final block without staging or importing again. See the host's logs".into(),
            });
        }
    };
    HttpResponse::Ok().json(TenantAcsImportResponse {
        party_id: body.party_id.clone(),
        seq: body.seq,
        complete: true,
        imported: body.total_len > 0 || body.seq > 1,
        marker_cleared,
    })
}

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

/// How the audit names a tenant caller: by the index of its key in
/// `DECPM_TENANT_API_KEYS`, never by the key.
fn tenant_requester(key_index: Option<usize>) -> String {
    match key_index {
        Some(index) => format!("tenant-key-{index}"),
        // Insecure/test mode checks no key.
        None => "tenant-key-unchecked".to_string(),
    }
}

/// Run one tenant write and record it in the audit, in a task of its own, and
/// return the write's answer unchanged.
///
/// A client that goes away drops its request, and actix drops the handler with
/// it. The task does not depend on the handler, so the write still finishes
/// and its row is still written. The task also catches a write that panics:
/// it answers 500 and writes the row itself.
async fn audited_write<W, F>(
    data: &web::Data<AppState>,
    key: Option<TenantKey>,
    endpoint: TenantEndpoint,
    party_id: String,
    write: W,
) -> HttpResponse
where
    W: FnOnce(web::Data<AppState>) -> F + Send + 'static,
    F: Future<Output = HttpResponse> + Send + 'static,
{
    let task = tokio::spawn({
        let data = data.clone();
        let party_id = party_id.clone();
        async move {
            let written = {
                let data = data.clone();
                AssertUnwindSafe(async move { write(data).await })
                    .catch_unwind()
                    .await
            };
            let response = written.unwrap_or_else(|_| {
                tracing::error!(
                    endpoint = endpoint.as_str(),
                    %party_id,
                    "a tenant write panicked"
                );
                write_failed()
            });
            let answer = Answer::of(response);
            record_answer(&data.db, key.as_ref(), endpoint, &party_id, &answer).await;
            answer
        }
    });
    match task.await {
        Ok(answer) => answer.into_response(),
        // The task caught the write's own panic and wrote its row, so this is a
        // failure of the audit insert itself. Another row could repeat that one.
        Err(e) => {
            tracing::error!(
                endpoint = endpoint.as_str(),
                %party_id,
                "a tenant write task did not complete: {e}"
            );
            write_failed()
        }
    }
}

/// The answer to a write that did not complete.
fn write_failed() -> HttpResponse {
    HttpResponse::InternalServerError().json(ErrorResponse {
        error: "The call failed on this host; see the host's logs".to_string(),
    })
}

/// A write's answer as plain data, so it can leave the task the write ran in.
struct Answer {
    status: actix_web::http::StatusCode,
    headers: actix_web::http::header::HeaderMap,
    body: actix_web::web::Bytes,
}

impl Answer {
    fn of(response: HttpResponse) -> Self {
        let (head, body) = response.into_parts();
        let body = body.try_into_bytes().unwrap_or_else(|_| {
            // The handlers here answer with JSON, which is never streamed.
            tracing::error!("a tenant write answered with a streamed body, which is dropped");
            actix_web::web::Bytes::new()
        });
        Self {
            status: head.status(),
            headers: head.headers().clone(),
            body,
        }
    }

    fn into_response(self) -> HttpResponse {
        let mut response = HttpResponse::with_body(self.status, self.body).map_into_boxed_body();
        *response.headers_mut() = self.headers;
        response
    }
}

/// Insert the audit row for a write's answer. The error text is kept only for
/// a refusal (4xx/5xx).
async fn record_answer(
    db: &sqlx::SqlitePool,
    key: Option<&TenantKey>,
    endpoint: TenantEndpoint,
    party_id: &str,
    answer: &Answer,
) {
    let error = (answer.status.is_client_error() || answer.status.is_server_error())
        .then(|| error_text(&answer.body))
        .flatten();
    let call = NewTenantCall {
        key_index: key.map(|k| k.index),
        key_id: key.map(|k| k.id.as_str()),
        endpoint,
        party_id: Some(party_id),
        http_status: Some(answer.status.as_u16()),
        error: error.as_deref(),
    };
    record(db, &call).await;
}

/// What an import that block 1 opens calls when it is retired: it records how
/// the copy ended against the key that opened it. No call answers at that
/// point, so the row has no HTTP status.
fn record_import_end(data: &AppState, key: Option<TenantKey>, party_id: &str) -> OnImportEnd {
    let db = data.db.clone();
    let party_id = party_id.to_string();
    Box::new(move |end| {
        Box::pin(async move {
            let (endpoint, error) = match &end {
                ImportEnd::Completed => (TenantEndpoint::AcsImportCompleted, None),
                ImportEnd::Abandoned(why) => {
                    (TenantEndpoint::AcsImportAbandoned, Some(why.as_str()))
                }
            };
            let call = NewTenantCall {
                key_index: key.as_ref().map(|k| k.index),
                key_id: key.as_ref().map(|k| k.id.as_str()),
                endpoint,
                party_id: Some(&party_id),
                http_status: None,
                error,
            };
            record(&db, &call).await;
        })
    })
}

/// Insert an audit row. A failure is logged and goes no further, because the
/// Canton write may already have happened and the caller must still see its
/// outcome.
async fn record(db: &sqlx::SqlitePool, call: &NewTenantCall<'_>) {
    if let Err(e) = tenant_call_audit::insert_call(db, call).await {
        tracing::error!(
            endpoint = call.endpoint.as_str(),
            http_status = call.http_status,
            "failed to record a tenant call in the audit: {e:#}"
        );
    }
}

/// The text of a refusal: the `error` field of an `ErrorResponse`, or the raw
/// body when it is not one.
fn error_text(body: &[u8]) -> Option<String> {
    let text = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|json| json.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    (!text.is_empty()).then_some(text)
}

/// Block size for draining and re-serving a snapshot.
const EXPORT_BLOCK_SIZE: usize = 1024 * 1024;

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

    let (status, serial) = match read_party_to_participant(&data.config, &body.party_id).await {
        Ok(Some(current)) if current.serial > base_serial => {
            (WorkflowProgress::Completed, current.serial)
        }
        Ok(Some(current)) => (WorkflowProgress::InProgress, current.serial),
        Ok(None) => (WorkflowProgress::InProgress, base_serial),
        Err(e) => {
            tracing::warn!("local-party adopt: post-submit status read failed: {e:#}");
            (WorkflowProgress::InProgress, base_serial)
        }
    };

    HttpResponse::Accepted().json(TenantAddHostsExecuteResponse {
        status,
        party_id: body.party_id.clone(),
        serial,
    })
}

// ============================================================================
// Helpers
// ============================================================================

/// `?base_serial=&seq=` on the ACS block endpoint.
#[derive(Debug, serde::Deserialize)]
pub struct AcsBlockQuery {
    /// Required rather than defaulted: it keys this replication, and guessing
    /// it would silently serve another attempt's export.
    pub base_serial: u32,
    /// Which block to serve, 1-based.
    pub seq: u64,
}

/// The 409 for a relay that would start while this host changes the party's
/// hosting.
fn relay_held_response(party_id: &str) -> HttpResponse {
    HttpResponse::Conflict().json(ErrorResponse {
        error: format!(
            "this host is leaving {party_id}, so no ACS relay for it may start; retry once the \
             unhost has finished"
        ),
    })
}

/// Base64-decode a raw Ed25519 public key into its fixed 32-byte array, or the
/// 400 response to return.
#[allow(clippy::result_large_err)]
fn decode_public_key(encoded: &str) -> std::result::Result<[u8; 32], HttpResponse> {
    let bytes = STANDARD.decode(encoded).map_err(|e| {
        HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("public_key is not valid base64: {e}"),
        })
    })?;
    bytes.try_into().map_err(|b: Vec<u8>| {
        HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("public_key must be 32 bytes, got {len}", len = b.len()),
        })
    })
}

/// Base64-decode every entry of `encoded`, or the 400 response to return.
/// `what` names the field in the error.
/// Returns the message rather than a built response: an `HttpResponse` in the
/// `Err` arm is an order of magnitude larger than the `Ok` value, which clippy
/// rightly objects to for a helper called on every request.
fn decode_all(encoded: &[String], what: &str) -> std::result::Result<Vec<Vec<u8>>, String> {
    encoded
        .iter()
        .map(|value| {
            STANDARD
                .decode(value)
                .map_err(|e| format!("{what} is not valid base64: {e}"))
        })
        .collect()
}

/// Map an [`AddHostsError`] onto the status code that tells the wallet what to
/// do about it: re-read and retry (409), fix the request (400/404), or treat
/// this host as unhealthy (500). Collapsing these into one code is the bug this
/// exists to prevent.
fn add_hosts_error_response(stage: &str, error: AddHostsError) -> HttpResponse {
    match error {
        AddHostsError::UnknownParty { .. } => {
            tracing::info!("tenant add-hosts {stage}: {error}");
            HttpResponse::NotFound().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        AddHostsError::StaleSerial { .. } => {
            tracing::info!("tenant add-hosts {stage}: {error}");
            HttpResponse::Conflict().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        AddHostsError::Busy(_) => {
            tracing::info!("tenant add-hosts {stage}: {error}");
            HttpResponse::Conflict().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        AddHostsError::Invalid(_) => {
            tracing::warn!("tenant add-hosts {stage}: refused the request: {error}");
            HttpResponse::BadRequest().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        AddHostsError::Canton(_) => {
            // The chain goes to the log, not the body. A tonic transport error
            // inside it names the admin endpoint address, and this response
            // crosses a tenant API boundary to a wallet provider — same class of
            // leak as omnibus#46. Nothing in the chain is actionable to the
            // caller anyway: a Canton failure is ours to fix.
            tracing::error!("tenant add-hosts {stage}: Canton call failed: {error:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: "A Canton call failed on this host; see the host's logs".to_string(),
            })
        }
    }
}

/// Map an [`ApproveHostingError`] onto the status code that tells the caller
/// what to do: retry later (404), stop because the proposal is stale or the
/// party exists (409), fix the request (400), or treat this host as unhealthy
/// (500).
fn approve_hosting_error_response(error: ApproveHostingError) -> HttpResponse {
    match error {
        ApproveHostingError::NotPending { .. } => {
            tracing::info!("decentralized-party hosting: {error}");
            HttpResponse::NotFound().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        ApproveHostingError::Conflict(_) => {
            tracing::info!("decentralized-party hosting: {error}");
            HttpResponse::Conflict().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        ApproveHostingError::Invalid(_) => {
            tracing::warn!("decentralized-party hosting: refused the request: {error}");
            HttpResponse::BadRequest().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        ApproveHostingError::Canton(_) => {
            // The chain goes to the log, not the body: a transport error in it
            // names the admin endpoint, and the caller cannot act on it.
            tracing::error!("decentralized-party hosting: Canton call failed: {error:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: "A Canton call failed on this host; see the host's logs".to_string(),
            })
        }
    }
}

#[cfg(test)]
async fn own_test_import(state: &AppState, party: &str) {
    MIGRATOR
        .run(&state.db)
        .await
        .expect("the test import uses the real schema");
    let target = replication_target(party, state.config.participant_id(), 1)
        .expect("the test party identifies a replication");
    sqlx::query("INSERT INTO tenant_import_window (participant_id, instance_name, party_id) VALUES (?, ?, ?)")
        .bind(state.config.participant_id().to_string())
        .bind(target.instance_name)
        .bind(party)
        .execute(&state.db).await.expect("the admitted test import owns its window");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_all_accepts_well_formed_base64() {
        let encoded = vec![STANDARD.encode([1u8, 2, 3]), STANDARD.encode([4u8, 5])];
        let Ok(decoded) = decode_all(&encoded, "signature") else {
            panic!("well-formed base64 must decode");
        };
        assert_eq!(decoded, vec![vec![1u8, 2, 3], vec![4u8, 5]]);
    }

    /// One bad entry fails the whole batch rather than being silently dropped —
    /// a dropped entry would break the index alignment the signatures rely on.
    #[test]
    fn decode_all_rejects_the_whole_batch_on_one_bad_entry() {
        let encoded = vec![STANDARD.encode([1u8]), "not base64!".to_string()];
        assert!(
            decode_all(&encoded, "signature").is_err(),
            "a batch with an undecodable entry must be refused whole"
        );
    }

    #[test]
    fn decode_all_maps_an_empty_input_to_an_empty_batch() {
        let Ok(decoded) = decode_all(&[], "signature") else {
            panic!("an empty batch must decode");
        };
        assert!(decoded.is_empty());
    }

    /// A full namespace fingerprint — `CantonId` deserializes through
    /// `Namespace::from_hex`, which refuses a truncated stub.
    fn test_participant() -> String {
        format!("participant-3::1220{}", "bb".repeat(32))
    }

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

    /// The bug this guards: one status code for every failure sends the wallet
    /// to the wrong remedy. A stale pin means "re-read and retry"; a refused
    /// bundle means "fix your request"; a Canton failure means "this host is
    /// unhealthy". They must not collapse.
    #[test]
    fn add_hosts_errors_map_to_distinct_status_codes() {
        let cases = [
            (
                AddHostsError::UnknownParty {
                    party: "alice::1220aa".to_string(),
                },
                actix_web::http::StatusCode::NOT_FOUND,
            ),
            (
                AddHostsError::StaleSerial {
                    party: "alice::1220aa".to_string(),
                    pinned: 4,
                    found: 5,
                },
                actix_web::http::StatusCode::CONFLICT,
            ),
            (
                AddHostsError::Invalid(anyhow::anyhow!("drops current host")),
                actix_web::http::StatusCode::BAD_REQUEST,
            ),
            (
                AddHostsError::Canton(anyhow::anyhow!("AddTransactions RPC failed")),
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, expected) in cases {
            let label = error.to_string();
            assert_eq!(
                add_hosts_error_response("prepare", error).status(),
                expected,
                "wrong status for {label}"
            );
        }
    }

    /// The audit names the key by its position, and must never carry the key.
    #[test]
    fn a_tenant_caller_is_recorded_by_key_index() {
        assert_eq!(tenant_requester(Some(2)), "tenant-key-2");
        assert_eq!(tenant_requester(None), "tenant-key-unchecked");
    }

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

    /// A stale-pin error must name both serials, or the wallet cannot tell what
    /// to re-read to.
    #[test]
    fn stale_serial_names_both_serials() {
        let message = AddHostsError::StaleSerial {
            party: "alice::1220aa".to_string(),
            pinned: 4,
            found: 7,
        }
        .to_string();
        assert!(message.contains('4'), "{message}");
        assert!(message.contains('7'), "{message}");
    }

    /// A party id that `replication_target` accepts.
    fn valid_party() -> String {
        format!("alice::1220{}", "aa".repeat(32))
    }

    /// The tenant key `"key"` against a budget of `limit` calls per minute, as
    /// a production node checks it, on a node with a participant id.
    fn meter(mut state: AppState, limit: u32) -> web::Data<AppState> {
        state.test_mode = false;
        state.config.tenant_api_keys = vec!["key".to_string()];
        state.config.tenant_rate_limit_per_minute = limit;
        state.config.node.participant_id =
            crate::canton_id::CantonId::parse(&test_participant()).ok();
        // Nothing listens here, so a Canton call fails at once.
        state.config.canton.admin_api_host = "127.0.0.1".to_string();
        state.config.canton.admin_api_port = 1;
        web::Data::new(state)
    }

    async fn metered_state(limit: u32) -> web::Data<AppState> {
        let Ok(state) = AppState::for_test(None).await else {
            panic!("the test state must build");
        };
        let Ok(state) = std::sync::Arc::try_unwrap(state.into_inner()) else {
            panic!("the freshly built state has no other owner");
        };
        meter(state, limit)
    }

    /// The instance name of the copy of `party` onto this node at serial 1.
    fn instance_of(state: &AppState, party: &str) -> String {
        match replication_target(party, state.config.participant_id(), 1) {
            Ok(target) => target.instance_name,
            Err(e) => panic!("the test party must name a replication: {e}"),
        }
    }

    /// Refused at validation, so it reaches no Canton.
    fn prepare_without_peers() -> actix_web::test::TestRequest {
        actix_web::test::TestRequest::post()
            .uri("/v0/tenant/prepare")
            .insert_header(("Authorization", "Bearer key"))
            .set_json(serde_json::json!({
                "party_hint": "alice",
                "public_key": STANDARD.encode([7u8; 32]),
                "hosting_peers": [],
            }))
    }

    fn import_block(party: &str, seq: u64, end: bool) -> actix_web::test::TestRequest {
        actix_web::test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .insert_header(("Authorization", "Bearer key"))
            .set_json(serde_json::json!({
                "party_id": party,
                "base_serial": 1,
                "seq": seq,
                "chunk": STANDARD.encode(b"block"),
                "end": end,
            }))
    }

    /// The middleware spends the budget, and the handler must not spend it a
    /// second time.
    #[actix_web::test]
    async fn a_call_is_charged_once() {
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(metered_state(2).await)
                .wrap(crate::server::middleware::AuthMiddleware)
                .service(tenant_prepare),
        )
        .await;

        for i in 0..2 {
            let response =
                actix_web::test::call_service(&app, prepare_without_peers().to_request()).await;
            assert_eq!(
                response.status(),
                actix_web::http::StatusCode::BAD_REQUEST,
                "call {i} is within a limit of 2"
            );
        }

        let third = actix_web::test::call_service(&app, prepare_without_peers().to_request()).await;
        assert_eq!(
            third.status(),
            actix_web::http::StatusCode::TOO_MANY_REQUESTS
        );
        assert!(
            third
                .headers()
                .get(actix_web::http::header::RETRY_AFTER)
                .is_some(),
            "a 429 must say how long to wait"
        );
        let body: serde_json::Value = actix_web::test::read_body_json(third).await;
        assert!(body["error"].is_string(), "{body}");
    }

    /// One ACS copy sends a block per call. The blocks of an open copy must
    /// not spend the budget the other calls share, and must pass when it is
    /// spent.
    #[actix_web::test]
    async fn the_blocks_of_an_open_copy_are_not_metered() {
        let state = metered_state(1).await;
        let party = valid_party();
        own_test_import(&state, &party).await;
        state
            .relay_sessions
            .open_test_import(&instance_of(&state, &party), None, Ok(()))
            .await;
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(state)
                .wrap(crate::server::middleware::AuthMiddleware)
                .service(tenant_prepare)
                .service(tenant_acs_import),
        )
        .await;

        for seq in 2..=4 {
            let response =
                actix_web::test::call_service(&app, import_block(&party, seq, false).to_request())
                    .await;
            assert_eq!(
                response.status(),
                actix_web::http::StatusCode::OK,
                "block {seq}"
            );
        }
        let prepare =
            actix_web::test::call_service(&app, prepare_without_peers().to_request()).await;
        assert_eq!(
            prepare.status(),
            actix_web::http::StatusCode::BAD_REQUEST,
            "the blocks must have left the whole budget"
        );

        let response =
            actix_web::test::call_service(&app, import_block(&party, 5, false).to_request()).await;
        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::OK,
            "a block of an open copy must pass with the budget spent"
        );
    }
}

/// Every request here is refused at validation, or reaches an import the test
/// opened without Canton, so the tests need no participant.
#[cfg(test)]
mod audit_tests {
    use actix_web::{App, http::StatusCode, test as actix_test};
    use serde_json::json;
    use sqlx::SqlitePool;

    use super::*;
    use crate::{
        canton_id::CantonId,
        db::MIGRATOR,
        server::{
            middleware::AuthMiddleware,
            tenant_call_audit::{PARTY_ID_CAP, TenantCallAuditRow},
        },
    };

    const PARTY: &str = "alice::1220aa";

    /// A party id that `replication_target` accepts.
    fn valid_party() -> String {
        format!("alice::1220{}", "aa".repeat(32))
    }

    /// The instance name of the copy of `party` onto this node at serial 1.
    fn instance_of(state: &AppState, party: &str) -> String {
        match replication_target(party, state.config.participant_id(), 1) {
            Ok(target) => target.instance_name,
            Err(e) => panic!("the test party must name a replication: {e}"),
        }
    }

    /// A state on `db` that checks the tenant keys `keys` against a budget of
    /// `limit` calls per minute, as a production node does.
    fn keyed_state(db: SqlitePool, keys: &[&str], limit: u32) -> web::Data<AppState> {
        let Ok(mut state) =
            std::sync::Arc::try_unwrap(AppState::for_test_on(None, db).into_inner())
        else {
            panic!("the freshly built state has no other owner");
        };
        state.test_mode = false;
        state.config.tenant_api_keys = keys.iter().map(ToString::to_string).collect();
        state.config.tenant_rate_limit_per_minute = limit;
        state.config.node.participant_id =
            CantonId::parse(&format!("participant-3::1220{}", "bb".repeat(32))).ok();
        // Nothing listens here, so a Canton call fails at once.
        state.config.canton.admin_api_host = "127.0.0.1".to_string();
        state.config.canton.admin_api_port = 1;
        web::Data::new(state)
    }

    async fn audit_rows(db: &SqlitePool) -> anyhow::Result<Vec<common::api::TenantCallAuditEntry>> {
        Ok(tenant_call_audit::list(db, None, 100)
            .await?
            .into_iter()
            .map(TenantCallAuditRow::into_entry)
            .collect())
    }

    /// One signature for two transactions, so the host refuses it with a 400.
    fn mismatched_add_hosts() -> TenantAddHostsExecuteRequest {
        TenantAddHostsExecuteRequest {
            party_id: PARTY.to_string(),
            base_serial: 4,
            topology_transactions: vec!["AQID".to_string(), "BAU=".to_string()],
            signatures: vec!["BAU=".to_string()],
            signed_by: "1220aa".to_string(),
        }
    }

    fn add_hosts_execute(key: &str) -> actix_test::TestRequest {
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/execute")
            .insert_header(("Authorization", format!("Bearer {key}")))
            .set_json(mismatched_add_hosts())
    }

    fn import(party_id: &str, seq: u64, chunk: &str, end: bool) -> actix_test::TestRequest {
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .insert_header(("Authorization", "Bearer key"))
            .set_json(json!({
                "party_id": party_id,
                "base_serial": 1,
                "seq": seq,
                "chunk": chunk,
                "end": end,
            }))
    }

    fn the_key() -> TenantKey {
        TenantKey {
            index: 0,
            id: crate::server::middleware::tenant_key_id("key"),
            dapp: false,
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_refused_add_hosts_execute_is_recorded_with_its_error(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 0);
        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .wrap(AuthMiddleware)
                .service(tenant_add_hosts_execute),
        )
        .await;

        let response = actix_test::call_service(&app, add_hosts_execute("key").to_request()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let content_type = response.headers().get("content-type").cloned();
        let body = actix_test::read_body(response).await;

        // The audit must hand the caller the handler's answer untouched.
        let direct = tenant_add_hosts_execute_on_this_host(&state, &mismatched_add_hosts()).await;
        assert_eq!(direct.headers().get("content-type").cloned(), content_type);
        let Ok(direct_body) = actix_web::body::to_bytes(direct.into_body()).await else {
            panic!("the handler's body must be readable");
        };
        assert_eq!(body, direct_body);

        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row.endpoint, "/v0/tenant/add-hosts/execute");
        assert_eq!(row.party_id.as_deref(), Some(PARTY));
        assert_eq!(row.http_status, Some(400));
        assert_eq!(row.key_index, Some(0));
        let body: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(row.error.as_deref(), body["error"].as_str());
        assert!(
            row.error
                .as_deref()
                .is_some_and(|e| e.contains("one signature per topology transaction")),
            "{:?}",
            row.error
        );
        Ok(())
    }

    /// A 401 returns before the handler body runs, so it leaves no row.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unauthenticated_call_leaves_no_row(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 0))
                .wrap(AuthMiddleware)
                .service(tenant_add_hosts_execute),
        )
        .await;

        let response =
            actix_test::call_service(&app, add_hosts_execute("not-a-key").to_request()).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(audit_rows(&db).await?.is_empty());
        Ok(())
    }

    /// The index changes when the key list is reordered, so the row also keeps
    /// the key's id, which does not. Neither is the key.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_key_is_named_by_index_and_id_and_not_stored(db: SqlitePool) -> anyhow::Result<()> {
        for keys in [["first", "second"], ["second", "first"]] {
            let app = actix_test::init_service(
                App::new()
                    .app_data(keyed_state(db.clone(), &keys, 0))
                    .wrap(AuthMiddleware)
                    .service(tenant_add_hosts_execute),
            )
            .await;
            actix_test::call_service(&app, add_hosts_execute("second").to_request()).await;
        }

        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 2, "{rows:?}");
        // Newest first: the reordered list put the key at index 0.
        assert_eq!(rows[0].key_index, Some(0));
        assert_eq!(rows[1].key_index, Some(1));
        let id = crate::server::middleware::tenant_key_id("second");
        assert_eq!(rows[0].key_id.as_deref(), Some(id.as_str()));
        assert_eq!(rows[1].key_id, rows[0].key_id);
        for row in &rows {
            let stored = serde_json::to_string(row)?;
            assert!(!stored.contains("second"), "{stored}");
        }
        Ok(())
    }

    /// Block 1 opens an import, so it is recorded. A final block that no open
    /// import takes is refused, and is recorded too. The blocks between only
    /// carry data.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_import_records_its_start_and_a_refused_end(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 0))
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;
        let party = valid_party();

        let data_block =
            actix_test::call_service(&app, import(&party, 2, "", false).to_request()).await;
        assert_eq!(data_block.status(), StatusCode::CONFLICT);
        assert!(audit_rows(&db).await?.is_empty());

        // A chunk that is not base64 stops block 1 before any Canton call.
        let start =
            actix_test::call_service(&app, import(&party, 1, "not base64!", false).to_request())
                .await;
        assert_eq!(start.status(), StatusCode::BAD_REQUEST);
        let end = actix_test::call_service(&app, import(&party, 3, "", true).to_request()).await;
        assert_eq!(end.status(), StatusCode::CONFLICT);

        let rows = audit_rows(&db).await?;
        let seen: Vec<(&str, Option<u16>)> = rows
            .iter()
            .map(|r| (r.endpoint.as_str(), r.http_status))
            .collect();
        assert_eq!(
            seen,
            [
                ("/v0/tenant/add-hosts/import (end)", Some(409)),
                ("/v0/tenant/add-hosts/import (start)", Some(400)),
            ]
        );
        assert!(
            rows.iter()
                .all(|r| r.party_id.as_deref() == Some(party.as_str()))
        );
        Ok(())
    }

    /// A block that no open copy takes spends the budget. Past it, the call
    /// gets a 429 and writes nothing.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_block_no_open_copy_takes_spends_the_budget(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 1))
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;

        let first = actix_test::call_service(&app, import(PARTY, 0, "", true).to_request()).await;
        assert_eq!(first.status(), StatusCode::BAD_REQUEST);
        let second = actix_test::call_service(&app, import(PARTY, 0, "", true).to_request()).await;
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        let stray =
            actix_test::call_service(&app, import(&valid_party(), 3, "", true).to_request()).await;
        assert_eq!(stray.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(audit_rows(&db).await?.len(), 1);
        Ok(())
    }

    /// A copy refused partway leaves the participant disconnected until the
    /// reaper ends it. So once block 1 is admitted, the final block must pass
    /// even when block 1 spent the last of the budget. The copy's teardown
    /// records how it ended.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_final_block_of_an_open_copy_spends_no_budget(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 1);
        let party = valid_party();
        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;

        // A real block 1 needs Canton to open the copy. A refused one spends
        // the budget the same way, and the test then opens the copy itself.
        let start =
            actix_test::call_service(&app, import(&party, 1, "not base64!", false).to_request())
                .await;
        assert_eq!(start.status(), StatusCode::BAD_REQUEST);
        let spent =
            actix_test::call_service(&app, import(&party, 1, "not base64!", false).to_request())
                .await;
        assert_eq!(spent.status(), StatusCode::TOO_MANY_REQUESTS);
        let instance = match replication_target(&party, state.config.participant_id(), 1) {
            Ok(target) => target.instance_name,
            Err(e) => panic!("the test party must name a replication: {e}"),
        };
        own_test_import(&state, &party).await;
        state
            .relay_sessions
            .open_test_import(
                &instance,
                Some(record_import_end(&state, Some(the_key()), &party)),
                Ok(()),
            )
            .await;

        let end = actix_test::call_service(&app, import(&party, 2, "", true).to_request()).await;

        assert_ne!(end.status(), StatusCode::TOO_MANY_REQUESTS);
        // The import finished. Without Canton the marker clear after it fails.
        let body: serde_json::Value = actix_test::read_body_json(end).await;
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.starts_with("Imported the ACS")),
            "{body}"
        );
        let rows = audit_rows(&db).await?;
        let seen: Vec<(&str, Option<u16>)> = rows
            .iter()
            .map(|r| (r.endpoint.as_str(), r.http_status))
            .collect();
        assert_eq!(
            seen,
            [
                ("/v0/tenant/add-hosts/import (end)", Some(500)),
                ("/v0/tenant/add-hosts/import (completed)", None),
                ("/v0/tenant/add-hosts/import (start)", Some(400)),
            ]
        );
        Ok(())
    }

    /// A block of an open copy that is not the next one is refused, so it is
    /// not part of the copy. It spends budget, and a refused final block gets
    /// its own row.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_wrong_block_of_an_open_copy_spends_budget_and_records_its_end(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 1);
        let party = valid_party();
        own_test_import(&state, &party).await;
        state
            .relay_sessions
            .open_test_import(&instance_of(&state, &party), None, Ok(()))
            .await;
        let app = actix_test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;

        let wrong_end =
            actix_test::call_service(&app, import(&party, 5, "", true).to_request()).await;
        assert_eq!(wrong_end.status(), StatusCode::CONFLICT);
        let wrong_data =
            actix_test::call_service(&app, import(&party, 6, "", false).to_request()).await;
        assert_eq!(wrong_data.status(), StatusCode::TOO_MANY_REQUESTS);

        let rows = audit_rows(&db).await?;
        let seen: Vec<(&str, Option<u16>)> = rows
            .iter()
            .map(|r| (r.endpoint.as_str(), r.http_status))
            .collect();
        assert_eq!(seen, [("/v0/tenant/add-hosts/import (end)", Some(409))]);
        Ok(())
    }

    /// The next block of an open copy passes with the budget spent, so a copy
    /// is never refused partway.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_next_block_of_an_open_copy_passes_with_the_budget_spent(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 1);
        let party = valid_party();
        own_test_import(&state, &party).await;
        state
            .relay_sessions
            .open_test_import(&instance_of(&state, &party), None, Ok(()))
            .await;
        let app = actix_test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;
        let chunk = STANDARD.encode(b"block");

        let wrong =
            actix_test::call_service(&app, import(&party, 7, &chunk, false).to_request()).await;
        assert_eq!(wrong.status(), StatusCode::CONFLICT);
        let spent =
            actix_test::call_service(&app, import(&party, 7, &chunk, false).to_request()).await;
        assert_eq!(spent.status(), StatusCode::TOO_MANY_REQUESTS);

        for seq in 2..=3 {
            let next =
                actix_test::call_service(&app, import(&party, seq, &chunk, false).to_request())
                    .await;
            assert_eq!(next.status(), StatusCode::OK, "block {seq}");
        }
        Ok(())
    }

    /// Block 1 spends budget before its chunk is checked. With the budget
    /// spent, a block 1 with a bad chunk gets a 429 and writes no row. Whether
    /// the chunk was decoded is not visible from outside, so this test does not
    /// check it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_spent_key_gets_429_for_block_1_and_no_row(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 1))
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;
        let party = valid_party();

        let first =
            actix_test::call_service(&app, import(&party, 1, "not base64!", false).to_request())
                .await;
        assert_eq!(first.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            audit_rows(&db).await?.len(),
            1,
            "block 1 gets its (start) row"
        );

        let spent =
            actix_test::call_service(&app, import(&party, 1, "not base64!", false).to_request())
                .await;
        assert_eq!(spent.status(), StatusCode::TOO_MANY_REQUESTS);
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "a 429 writes no row: {rows:?}");
        assert_eq!(rows[0].endpoint, "/v0/tenant/add-hosts/import (start)");
        Ok(())
    }

    /// A next block that fails the checks cannot be imported, so it is not
    /// part of the copy: it spends budget, and a failed final block gets its
    /// own row. A valid next block still passes with the budget spent.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_next_block_that_fails_its_checks_spends_budget(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 1);
        let party = valid_party();
        own_test_import(&state, &party).await;
        state
            .relay_sessions
            .open_test_import(&instance_of(&state, &party), None, Ok(()))
            .await;
        let app = actix_test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;

        let bad_end =
            actix_test::call_service(&app, import(&party, 2, "not base64!", true).to_request())
                .await;
        assert_eq!(bad_end.status(), StatusCode::BAD_REQUEST);
        let bad_data =
            actix_test::call_service(&app, import(&party, 2, "not base64!", false).to_request())
                .await;
        assert_eq!(bad_data.status(), StatusCode::TOO_MANY_REQUESTS);
        let good = actix_test::call_service(
            &app,
            import(&party, 2, &STANDARD.encode(b"block"), false).to_request(),
        )
        .await;
        assert_eq!(good.status(), StatusCode::OK);

        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].endpoint, "/v0/tenant/add-hosts/import (end)");
        assert_eq!(rows[0].http_status, Some(400));
        assert!(
            rows[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("not valid base64")),
            "{:?}",
            rows[0].error
        );
        Ok(())
    }

    /// The write runs in its own task. When it panics after its caller went
    /// away, the task still writes its row, once.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_write_that_panics_after_its_caller_went_away_is_recorded_once(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 0);
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let call = audited_write(
            &state,
            Some(the_key()),
            TenantEndpoint::ThresholdExecute,
            PARTY.to_string(),
            move |_| async move {
                let _ = finished.await;
                a_broken_write()
            },
        );

        let gone = tokio::time::timeout(std::time::Duration::from_millis(50), call).await;
        assert!(
            gone.is_err(),
            "the write must still run when the caller goes"
        );
        let _ = finish.send(());

        for _ in 0..200 {
            if !audit_rows(&db).await?.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // A second row would come at once, from the same task.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].http_status, Some(500));
        Ok(())
    }

    /// A client that goes away drops the handler. The write runs on in its own
    /// task, so its row is still written.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_write_whose_caller_went_away_is_still_recorded(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 0);
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let call = audited_write(
            &state,
            Some(the_key()),
            TenantEndpoint::AddHostsExecute,
            PARTY.to_string(),
            move |_| async move {
                // Stands for a Canton write that is still running.
                let _ = finished.await;
                HttpResponse::Conflict().json(ErrorResponse {
                    error: "the pinned serial moved".to_string(),
                })
            },
        );

        let gone = tokio::time::timeout(std::time::Duration::from_millis(50), call).await;
        assert!(
            gone.is_err(),
            "the write must still run when the caller goes"
        );
        assert!(audit_rows(&db).await?.is_empty());
        let _ = finish.send(());

        let mut rows = Vec::new();
        for _ in 0..200 {
            rows = audit_rows(&db).await?;
            if !rows.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].endpoint, "/v0/tenant/add-hosts/execute");
        assert_eq!(rows[0].http_status, Some(409));
        assert_eq!(rows[0].error.as_deref(), Some("the pinned serial moved"));
        assert_eq!(rows[0].key_id, Some(the_key().id));
        Ok(())
    }

    fn a_broken_write() -> HttpResponse {
        panic!("the write broke")
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_write_that_panics_answers_500_and_is_recorded(db: SqlitePool) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 0);

        let response = audited_write(
            &state,
            Some(the_key()),
            TenantEndpoint::ThresholdExecute,
            PARTY.to_string(),
            move |_| async move { a_broken_write() },
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].endpoint, "/v0/tenant/threshold/execute");
        assert_eq!(rows[0].http_status, Some(500));
        assert!(
            rows[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("failed on this host")),
            "{:?}",
            rows[0].error
        );
        Ok(())
    }

    /// The JSON limit is 100 MiB, and the caller chooses the party id.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_over_long_party_id_is_stored_capped(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 0))
                .wrap(AuthMiddleware)
                .service(tenant_acs_import),
        )
        .await;
        let party = "p".repeat(100_000);

        let response =
            actix_test::call_service(&app, import(&party, 0, "", true).to_request()).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        let Some(stored) = rows[0].party_id.as_deref() else {
            panic!("the row must keep the capped party id");
        };
        assert!(stored.starts_with(&"p".repeat(PARTY_ID_CAP)), "{stored}");
        assert!(stored.ends_with("[truncated]"), "{stored}");
        assert!(stored.chars().count() < PARTY_ID_CAP + 20, "{stored}");
        Ok(())
    }

    /// The end of an import that block 1 opened is recorded against the key
    /// that opened it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_end_of_an_import_is_recorded_against_its_key(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = keyed_state(db.clone(), &["key"], 0);

        record_import_end(&state, Some(the_key()), PARTY)(ImportEnd::Completed).await;
        record_import_end(&state, Some(the_key()), PARTY)(ImportEnd::Abandoned(
            "the reaper ended the idle copy: the wallet stopped relaying".to_string(),
        ))
        .await;

        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 2, "{rows:?}");
        let (abandoned, completed) = (&rows[0], &rows[1]);
        assert_eq!(
            abandoned.endpoint,
            "/v0/tenant/add-hosts/import (abandoned)"
        );
        assert!(
            abandoned
                .error
                .as_deref()
                .is_some_and(|e| e.contains("reaper")),
            "{:?}",
            abandoned.error
        );
        assert_eq!(
            completed.endpoint,
            "/v0/tenant/add-hosts/import (completed)"
        );
        assert_eq!(completed.error, None);
        for row in &rows {
            assert_eq!(row.party_id.as_deref(), Some(PARTY));
            assert_eq!(row.http_status, None, "no call answers an import's end");
            assert_eq!(row.key_index, Some(0));
            assert_eq!(row.key_id, Some(the_key().id));
        }
        Ok(())
    }

    /// An execute names no party id. The row holds the one the call would
    /// create, so the audit can be read by party.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn execute_records_the_party_the_call_named(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 0))
                .wrap(AuthMiddleware)
                .service(tenant_execute),
        )
        .await;
        // `signed_by` is not the fingerprint of the key, so the host refuses it.
        let request = actix_test::TestRequest::post()
            .uri("/v0/tenant/execute")
            .insert_header(("Authorization", "Bearer key"))
            .set_json(json!({
                "party_hint": "alice",
                "public_key": STANDARD.encode([7u8; 32]),
                "topology_transactions": [],
                "signatures": [],
                "signed_by": "1220ff",
            }));

        let response = actix_test::call_service(&app, request.to_request()).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].endpoint, "/v0/tenant/execute");
        assert_eq!(rows[0].party_id.as_deref(), Some("alice::1220ff"));
        Ok(())
    }

    /// The Canton write may already have happened, so a broken audit must not
    /// change what the caller sees.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_audit_write_does_not_fail_the_call(db: SqlitePool) -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(keyed_state(db.clone(), &["key"], 0))
                .wrap(AuthMiddleware)
                .service(tenant_add_hosts_execute),
        )
        .await;
        let recorded = actix_test::call_service(&app, add_hosts_execute("key").to_request()).await;
        assert_eq!(recorded.status(), StatusCode::BAD_REQUEST);
        let recorded = actix_test::read_body(recorded).await;

        sqlx::query("DROP TABLE tenant_call_audit")
            .execute(&db)
            .await?;
        let unrecorded =
            actix_test::call_service(&app, add_hosts_execute("key").to_request()).await;

        assert_eq!(unrecorded.status(), StatusCode::BAD_REQUEST);
        assert_eq!(actix_test::read_body(unrecorded).await, recorded);
        Ok(())
    }
}

#[cfg(test)]
#[path = "tenant_relay_audit_tests.rs"]
mod relay_route_audit_tests;

/// The hosting approval against a fake participant. Canton is the only thing
/// faked: the middleware, the handler, the checks and the audit run for real.
#[cfg(test)]
mod approve_hosting_tests {
    use std::sync::Arc;

    use actix_web::{App, http::StatusCode, test as actix_test};
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::{Signature, SigningKeysWithThreshold},
        protocol::v30::{
            DecentralizedNamespaceDefinition, MultiTransactionSignatures, PartyToParticipant,
            SignedTopologyTransaction, TopologyMapping, TopologyTransaction,
            enums::{ParticipantPermission, TopologyChangeOp},
            party_to_participant::HostingParticipant,
            topology_mapping,
        },
        version::v1::{UntypedVersionedMessage, untyped_versioned_message},
    };
    use prost::Message as _;
    use serde_json::json;
    use sqlx::SqlitePool;

    use super::*;
    use crate::{
        db::MIGRATOR,
        server::{middleware::AuthMiddleware, tenant_call_audit::TenantCallAuditRow},
        signing::{
            custody::SoftwareKeys,
            fake_admin_api::{FakeAdminApi, TestNode, VaultKey},
            key_store::topology_transaction_hash,
        },
        workflow::external_party::steps::party_signing_key,
    };

    const ROUTE: &str = "/v0/tenant/decentralized-party/approve-hosting";

    fn namespace() -> String {
        format!("1220{}", "dd".repeat(32))
    }

    fn party() -> String {
        format!("dapp-party::{}", namespace())
    }

    fn other_host() -> String {
        format!("participant-2::1220{}", "02".repeat(32))
    }

    fn confirming(host: &str) -> HostingParticipant {
        HostingParticipant {
            participant_uid: host.to_string(),
            permission: ParticipantPermission::Confirmation as i32,
            onboarding: None,
        }
    }

    /// A party with two owner keys at threshold 2, hosted on `hosts` at
    /// confirmation threshold 1.
    fn onboarding(hosts: &[&str]) -> PartyToParticipant {
        PartyToParticipant {
            party: party(),
            threshold: 1,
            participants: hosts.iter().map(|host| confirming(host)).collect(),
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: vec![party_signing_key(&[1; 32]), party_signing_key(&[2; 32])],
                threshold: 2,
            }),
        }
    }

    /// The proposal as the owners' participants leave it in the store: the
    /// owners signed the whole onboarding bundle with one signature each.
    fn proposal(mapping: PartyToParticipant) -> SignedTopologyTransaction {
        let transaction = UntypedVersionedMessage {
            version: 30,
            wrapper: Some(untyped_versioned_message::Wrapper::Data(
                TopologyTransaction {
                    operation: TopologyChangeOp::AddReplace as i32,
                    serial: 1,
                    mapping: Some(TopologyMapping {
                        mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
                    }),
                }
                .encode_to_vec(),
            )),
        }
        .encode_to_vec();
        SignedTopologyTransaction {
            multi_transaction_signatures: vec![MultiTransactionSignatures {
                transaction_hashes: vec![topology_transaction_hash(&transaction)],
                signatures: vec![Signature {
                    signed_by: format!("1220{}", "e1".repeat(32)),
                    signature: vec![1; 64],
                    ..Default::default()
                }],
            }],
            transaction,
            signatures: vec![],
            proposal: true,
        }
    }

    fn hash_of(proposal: &SignedTopologyTransaction) -> String {
        STANDARD.encode(topology_transaction_hash(&proposal.transaction))
    }

    /// The party's namespace, owned by two keys outside this node.
    fn definition() -> DecentralizedNamespaceDefinition {
        owned_by(format!("1220{}", "e1".repeat(32)))
    }

    /// The party's namespace, owned by `owner` and one key outside this node.
    fn owned_by(owner: String) -> DecentralizedNamespaceDefinition {
        DecentralizedNamespaceDefinition {
            decentralized_namespace: namespace(),
            threshold: 2,
            owners: vec![owner, format!("1220{}", "e2".repeat(32))],
        }
    }

    /// A node on a fake participant that checks the tenant key `"key"` and the
    /// dApp key `"dapp-key"` against a budget of `limit` calls per minute.
    struct Host {
        fake: FakeAdminApi,
        _node: TestNode,
        state: web::Data<AppState>,
    }

    impl Host {
        async fn start(db: SqlitePool, limit: u32) -> anyhow::Result<Self> {
            Self::with_vault(db, limit, vec![]).await
        }

        /// A host whose participant's vault holds `keys`.
        async fn with_vault(
            db: SqlitePool,
            limit: u32,
            keys: Vec<VaultKey>,
        ) -> anyhow::Result<Self> {
            let fake = FakeAdminApi::start(keys).await?;
            let node = fake.node(SoftwareKeys::Refused, false).await?;
            let Ok(mut state) = Arc::try_unwrap(AppState::for_test_on(None, db).into_inner())
            else {
                panic!("the freshly built state has no other owner");
            };
            state.test_mode = false;
            state.config = node.config.clone();
            state.config.tenant_api_keys = vec!["key".to_string()];
            state.config.dapp_api_keys = vec!["dapp-key".to_string()];
            state.config.tenant_rate_limit_per_minute = limit;
            Ok(Self {
                fake,
                _node: node,
                state: web::Data::new(state),
            })
        }

        fn this_host(&self) -> String {
            self.state.config.participant_id().to_string()
        }

        /// The onboarding this host co-validates, hosted here and on one other
        /// participant.
        fn onboarding(&self) -> PartyToParticipant {
            onboarding(&[&self.this_host(), &other_host()])
        }

        async fn call(&self, key: &str, hash: &str) -> (StatusCode, serde_json::Value) {
            let app = actix_test::init_service(
                App::new()
                    .app_data(self.state.clone())
                    .wrap(AuthMiddleware)
                    .service(tenant_approve_decentralized_hosting),
            )
            .await;
            let response =
                actix_test::call_service(&app, request(key, &party(), hash).to_request()).await;
            let status = response.status();
            let body = actix_test::read_body(response).await;
            let body = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            (status, body)
        }
    }

    fn request(key: &str, party_id: &str, hash: &str) -> actix_test::TestRequest {
        actix_test::TestRequest::post()
            .uri(ROUTE)
            .insert_header(("Authorization", format!("Bearer {key}")))
            .set_json(json!({ "party_id": party_id, "transaction_hash": hash }))
    }

    async fn audit_rows(db: &SqlitePool) -> anyhow::Result<Vec<common::api::TenantCallAuditEntry>> {
        Ok(tenant_call_audit::list(db, None, 100)
            .await?
            .into_iter()
            .map(TenantCallAuditRow::into_entry)
            .collect())
    }

    /// The one audit row a call left, after checking it names the route, the
    /// party and `status`.
    async fn only_row(
        db: &SqlitePool,
        status: StatusCode,
    ) -> anyhow::Result<common::api::TenantCallAuditEntry> {
        let mut rows = audit_rows(db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = rows.remove(0);
        assert_eq!(row.endpoint, ROUTE);
        assert_eq!(row.party_id.as_deref(), Some(party().as_str()));
        assert_eq!(row.http_status, Some(status.as_u16()));
        assert_eq!(row.key_index, Some(0));
        Ok(row)
    }

    /// The host finds the proposal by its hash, signs exactly the stored
    /// transaction with its own key, and submits it. The owners' signatures
    /// on the whole bundle travel with it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn signs_and_submits_the_pending_proposal_with_the_requested_hash(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        host.fake.publish_namespace(1, definition());
        let pending = proposal(host.onboarding());
        host.fake.propose(pending.clone());

        let (status, body) = host.call("key", &hash_of(&pending)).await;

        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        // Still a proposal here until the other host signs.
        assert_eq!(body, json!({ "status": "inprogress", "party_id": party() }));
        let signed = host.fake.sign_requests();
        assert_eq!(signed.len(), 1, "{signed:?}");
        assert_eq!(signed[0].transactions, vec![pending.clone()]);
        assert!(signed[0].signed_by.is_empty(), "the node picks its own key");
        let added = host.fake.add_requests();
        assert_eq!(added.len(), 1, "{added:?}");
        assert_eq!(added[0].transactions, vec![pending]);
        let row = only_row(&db, StatusCode::ACCEPTED).await?;
        assert_eq!(row.error, None);
        Ok(())
    }

    /// A retry after the party went in force signs nothing again.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_this_host_already_hosts_answers_completed(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        host.fake.publish_namespace(1, definition());
        host.fake.publish_p2p(1, host.onboarding());

        let (status, body) = host
            .call("key", &hash_of(&proposal(host.onboarding())))
            .await;

        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body, json!({ "status": "completed", "party_id": party() }));
        assert!(host.fake.sign_requests().is_empty());
        assert!(host.fake.add_requests().is_empty());
        only_row(&db, StatusCode::ACCEPTED).await?;
        Ok(())
    }

    /// The owners' submission has not reached this host yet, so the caller
    /// retries.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn no_pending_proposal_answers_404(db: SqlitePool) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        host.fake.publish_namespace(1, definition());

        let (status, body) = host
            .call("key", &hash_of(&proposal(host.onboarding())))
            .await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(host.fake.sign_requests().is_empty());
        let row = only_row(&db, StatusCode::NOT_FOUND).await?;
        assert_eq!(row.error.as_deref(), body["error"].as_str());
        Ok(())
    }

    /// The proposal that names this host is not the one the caller asked for.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_pending_proposal_with_another_hash_answers_409(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        host.fake.publish_namespace(1, definition());
        host.fake.propose(proposal(host.onboarding()));
        let mut replaced = host.onboarding();
        if let Some(keys) = replaced.party_signing_keys.as_mut() {
            keys.threshold = 1;
        }

        let (status, body) = host.call("key", &hash_of(&proposal(replaced))).await;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(host.fake.sign_requests().is_empty());
        let row = only_row(&db, StatusCode::CONFLICT).await?;
        assert!(
            row.error
                .as_deref()
                .is_some_and(|e| e.contains("none has the requested hash")),
            "{row:?}"
        );
        Ok(())
    }

    /// Onboarding creates a party. A party already in force with other content
    /// is not signed for again, even when the proposal matches.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_in_force_with_other_content_answers_409(db: SqlitePool) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        host.fake.publish_namespace(1, definition());
        let third = format!("participant-3::1220{}", "03".repeat(32));
        host.fake
            .publish_p2p(1, onboarding(&[&other_host(), &third]));
        let pending = proposal(host.onboarding());
        host.fake.propose(pending.clone());

        let (status, body) = host.call("key", &hash_of(&pending)).await;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(host.fake.sign_requests().is_empty());
        let row = only_row(&db, StatusCode::CONFLICT).await?;
        assert!(
            row.error
                .as_deref()
                .is_some_and(|e| e.contains("already exists")),
            "{row:?}"
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_proposal_that_fails_a_check_answers_400_and_is_not_signed(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        host.fake.publish_namespace(1, definition());
        let mut submitting = host.onboarding();
        submitting.participants[0].permission = ParticipantPermission::Submission as i32;
        let pending = proposal(submitting);
        host.fake.propose(pending.clone());

        let (status, body) = host.call("key", &hash_of(&pending)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(host.fake.sign_requests().is_empty());
        assert!(host.fake.add_requests().is_empty());
        let row = only_row(&db, StatusCode::BAD_REQUEST).await?;
        assert!(
            row.error
                .as_deref()
                .is_some_and(|e| e.contains("Confirmation")),
            "{row:?}"
        );
        Ok(())
    }

    /// A covalidator is not an owner. When the vault holds a key that owns
    /// the namespace, the node's signature would also be that owner's, so the
    /// node refuses before it signs or submits anything.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_namespace_this_node_co_owns_is_refused_before_signing(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let owner_key = VaultKey::software("dapp-party-namespace", 9);
        let host = Host::with_vault(db.clone(), 0, vec![owner_key.clone()]).await?;
        host.fake
            .publish_namespace(1, owned_by(owner_key.fingerprint().to_string()));
        let pending = proposal(host.onboarding());
        host.fake.propose(pending.clone());

        let (status, body) = host.call("key", &hash_of(&pending)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(host.fake.sign_requests().is_empty());
        assert!(host.fake.add_requests().is_empty());
        let row = only_row(&db, StatusCode::BAD_REQUEST).await?;
        assert!(
            row.error
                .as_deref()
                .is_some_and(|e| e.contains(owner_key.fingerprint().as_str())
                    && e.contains("owns the namespace")),
            "{row:?}"
        );
        Ok(())
    }

    /// The node can sign for its own participant namespace even when the
    /// root key is not in the vault, so that namespace counts as held too.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_namespace_its_participant_co_owns_is_refused(db: SqlitePool) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        let participant_namespace = host.state.config.participant_id().namespace.to_hex();
        host.fake
            .publish_namespace(1, owned_by(participant_namespace));
        let pending = proposal(host.onboarding());
        host.fake.propose(pending.clone());

        let (status, body) = host.call("key", &hash_of(&pending)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(host.fake.sign_requests().is_empty());
        assert!(host.fake.add_requests().is_empty());
        Ok(())
    }

    /// Keys of its own that own nothing here do not stop the node.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_namespace_owned_only_by_outside_keys_still_signs(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let unrelated = VaultKey::software("another-party-namespace", 9);
        let host = Host::with_vault(db.clone(), 0, vec![unrelated]).await?;
        host.fake.publish_namespace(1, definition());
        let pending = proposal(host.onboarding());
        host.fake.propose(pending.clone());

        let (status, body) = host.call("key", &hash_of(&pending)).await;

        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(host.fake.sign_requests().len(), 1);
        assert_eq!(host.fake.add_requests().len(), 1);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_without_a_decentralized_namespace_answers_400(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        let pending = proposal(host.onboarding());
        host.fake.propose(pending.clone());

        let (status, body) = host.call("key", &hash_of(&pending)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(host.fake.sign_requests().is_empty());
        let row = only_row(&db, StatusCode::BAD_REQUEST).await?;
        assert!(
            row.error
                .as_deref()
                .is_some_and(|e| e.contains("DecentralizedNamespaceDefinition")),
            "{row:?}"
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_hash_that_is_not_base64_answers_400(db: SqlitePool) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;

        let (status, body) = host.call("key", "not base64!").await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let row = only_row(&db, StatusCode::BAD_REQUEST).await?;
        assert!(
            row.error
                .as_deref()
                .is_some_and(|e| e.contains("transaction_hash")),
            "{row:?}"
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_id_that_does_not_parse_answers_400(db: SqlitePool) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(host.state.clone())
                .wrap(AuthMiddleware)
                .service(tenant_approve_decentralized_hosting),
        )
        .await;

        let response = actix_test::call_service(
            &app,
            request("key", "no-namespace", &STANDARD.encode([0u8; 34])).to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let rows = audit_rows(&db).await?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].party_id.as_deref(), Some("no-namespace"));
        Ok(())
    }

    /// A participant that cannot be reached is this host's problem, and the
    /// answer does not say where the participant is.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_canton_call_answers_500(db: SqlitePool) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        let Ok(mut state) = Arc::try_unwrap(host.state.into_inner()) else {
            panic!("the test holds the only reference to its state");
        };
        // Nothing listens here, so the first Canton call fails at once.
        state.config.canton.admin_api_port = 1;
        let host = Host {
            state: web::Data::new(state),
            ..host
        };

        let (status, body) = host
            .call("key", &hash_of(&proposal(host.onboarding())))
            .await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(
            body["error"],
            json!("A Canton call failed on this host; see the host's logs")
        );
        only_row(&db, StatusCode::INTERNAL_SERVER_ERROR).await?;
        Ok(())
    }

    /// A dApp key reaches the approval, and the row names it. The same key
    /// gets a 403 from another tenant route, before that handler runs.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_dapp_key_reaches_the_approval_and_no_other_write(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;
        host.fake.publish_namespace(1, definition());
        let pending = proposal(host.onboarding());
        host.fake.propose(pending.clone());

        let (status, body) = host.call("dapp-key", &hash_of(&pending)).await;

        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let row = only_row(&db, StatusCode::ACCEPTED).await?;
        assert_eq!(
            row.key_id,
            Some(crate::server::middleware::tenant_key_id("dapp-key"))
        );

        let app = actix_test::init_service(
            App::new()
                .app_data(host.state.clone())
                .wrap(AuthMiddleware)
                .service(tenant_execute),
        )
        .await;
        let execute = actix_test::TestRequest::post()
            .uri("/v0/tenant/execute")
            .insert_header(("Authorization", "Bearer dapp-key"))
            .set_json(json!({
                "party_hint": "alice",
                "public_key": STANDARD.encode([7u8; 32]),
                "topology_transactions": [],
                "signatures": [],
                "signed_by": "1220ff",
            }));
        let refused = actix_test::call_service(&app, execute.to_request()).await;

        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert_eq!(audit_rows(&db).await?.len(), 1, "a 403 writes no row");
        Ok(())
    }

    /// A 401 returns before the handler runs, so it leaves no row.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_wrong_key_answers_401(db: SqlitePool) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 0).await?;

        let (status, _) = host.call("not-a-key", "AA==").await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(audit_rows(&db).await?.is_empty());
        assert!(host.fake.sign_requests().is_empty());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_key_past_its_budget_answers_429(db: SqlitePool) -> anyhow::Result<()> {
        let host = Host::start(db.clone(), 1).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(host.state.clone())
                .wrap(AuthMiddleware)
                .service(tenant_approve_decentralized_hosting),
        )
        .await;

        let first =
            actix_test::call_service(&app, request("key", &party(), "AA==").to_request()).await;
        assert_eq!(first.status(), StatusCode::NOT_FOUND);
        let limited =
            actix_test::call_service(&app, request("key", &party(), "AA==").to_request()).await;

        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            limited
                .headers()
                .get(actix_web::http::header::RETRY_AFTER)
                .is_some(),
            "a 429 must say how long to wait"
        );
        assert_eq!(audit_rows(&db).await?.len(), 1, "a 429 writes no row");
        Ok(())
    }
}
