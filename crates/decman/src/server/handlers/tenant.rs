//! Wallet-facing tenant API (`/v0/tenant/*`) — external-party onboarding.
//!
//! These endpoints let a wallet provider drive external-party onboarding for a
//! party whose Ed25519 namespace key the wallet generates and holds. DPM only
//! relays: it prepares the onboarding topology, and submits the wallet-signed
//! bundle to its own participant. Every binary field on the wire is base64
//! (STANDARD engine).
//!
//! Onboarding is all this API does. Transacting as the party — reading its
//! contracts, preparing and executing submissions, moving token-standard assets —
//! is the wallet's own job against Canton, because it needs a Ledger-API
//! credential the node has no business holding on the party's behalf. Onboarding,
//! by contrast, writes topology over the tokenless Admin API and needs no ledger
//! token at all.
//!
//! Unlike the admin endpoints (Keycloak JWT + `require_admin`), tenant callers
//! authenticate with a provider-issued API key, so every handler here calls
//! [`require_tenant_api_key`] first (the `/v0/tenant/` prefix bypasses the JWT
//! middleware).

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    canton_id::validate_party_id_prefix,
    server::{
        AppState,
        middleware::require_tenant_api_key,
        types::{
            ErrorResponse, TenantAddHostsOnboardRequest, TenantAddHostsOnboardResponse,
            TenantAddHostsPrepareResponse, TenantAddHostsRequest, TenantOnboardRequest,
            TenantOnboardResponse, TenantPrepareRequest, TenantPrepareResponse, WorkflowProgress,
            WorkflowStatusResponse,
        },
    },
    workflow::external_party::{
        add_hosts::{
            AddHostsError, ExternalPartyAddHostsPayload, prepare_add_hosts,
            read_party_to_participant, submit_add_hosts,
        },
        keys::fingerprint_from_public_key,
        steps::{
            ExternalPartyAllocatePayload, HostOnboardingStatus, allocate_party,
            host_onboarding_status, prepare_topology,
        },
    },
};

use super::workflows::validate_confirmation_threshold;

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
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/prepare")]
pub async fn tenant_prepare(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantPrepareRequest>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
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
    // `/v0/tenant/onboard` next.
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
                error: format!("Failed to prepare onboarding topology: {e}"),
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
    request_body = TenantOnboardRequest,
    responses(
        (status = 202, description = "Allocated on this host (status reflects this host's view)", body = TenantOnboardResponse),
        (status = 400, description = "Bad request (bad base64, or signed_by != public_key fingerprint)", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 500, description = "Allocation failed on this participant", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/onboard")]
pub async fn tenant_onboard(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantOnboardRequest>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }
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

    // Submit on THIS participant only. The wallet calls `/onboard` on every host
    // itself; no host relays to another. Canton keeps the topology a proposal until
    // every host has authorized it, and re-submitting an identical transaction is a
    // no-op, so a wallet retry converges.
    if let Err(e) = allocate_party(&data.config, &bundle).await {
        tracing::error!("tenant onboard: allocate on this participant failed: {e:#}");
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to allocate external party on this host: {e}"),
        });
    }

    // Report this host's view: `Completed` once its authorized mapping names it,
    // else `InProgress` (still a proposal here — other hosts must sign).
    let status = match host_onboarding_status(&data.config, &party_id).await {
        Ok(HostOnboardingStatus::Hosted) => WorkflowProgress::Completed,
        Ok(_) => WorkflowProgress::InProgress,
        Err(e) => {
            tracing::warn!("tenant onboard: post-allocate status read failed: {e:#}");
            WorkflowProgress::InProgress
        }
    };
    HttpResponse::Accepted().json(TenantOnboardResponse { status, party_id })
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
        (status = 404, description = "This host does not host this party", body = ErrorResponse)
    )
)]
#[get("/v0/tenant/{party}/status")]
pub async fn tenant_status(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
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
        Ok(HostOnboardingStatus::Absent) => HttpResponse::NotFound().json(ErrorResponse {
            error: format!("This host does not host external party {party}"),
        }),
        Err(e) => {
            tracing::error!("tenant status: topology read failed: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to read onboarding status: {e}"),
            })
        }
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
        (status = 404, description = "This host does not host this party", body = ErrorResponse),
        (status = 409, description = "This host reads a different serial for the party", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this host", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/add-hosts/prepare")]
pub async fn tenant_add_hosts_prepare(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantAddHostsRequest>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
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
    match prepare_add_hosts(
        &data.config,
        &body.party_id,
        &body.new_hosts,
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
        Err(e) => add_hosts_error_response("prepare", e),
    }
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
    request_body = TenantAddHostsOnboardRequest,
    responses(
        (status = 202, description = "Submitted on this host", body = TenantAddHostsOnboardResponse),
        (status = 400, description = "Bad request, or topology that is not a plain add-hosts", body = ErrorResponse),
        (status = 401, description = "Invalid tenant API key", body = ErrorResponse),
        (status = 404, description = "This host does not host this party", body = ErrorResponse),
        (status = 409, description = "The pinned base serial has moved on this host", body = ErrorResponse),
        (status = 500, description = "A Canton call failed on this host", body = ErrorResponse)
    )
)]
#[post("/v0/tenant/add-hosts/onboard")]
pub async fn tenant_add_hosts_onboard(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<TenantAddHostsOnboardRequest>,
) -> impl Responder {
    if let Err(resp) = require_tenant_api_key(&http_req, &data) {
        return resp;
    }

    let topology_transactions =
        match decode_all(&body.topology_transactions, "topology transaction") {
            Ok(v) => v,
            Err(resp) => return resp,
        };
    let signatures = match decode_all(&body.signatures, "signature") {
        Ok(v) => v,
        Err(resp) => return resp,
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
        Err(e) => return add_hosts_error_response("onboard", e),
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
            tracing::warn!("tenant add-hosts onboard: post-submit status read failed: {e:#}");
            (WorkflowProgress::InProgress, base_serial)
        }
    };

    HttpResponse::Accepted().json(TenantAddHostsOnboardResponse {
        status,
        party_id: body.party_id.clone(),
        serial,
    })
}

// ============================================================================
// Helpers
// ============================================================================

/// Base64-decode a raw Ed25519 public key into its fixed 32-byte array, or the
/// 400 response to return.
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
fn decode_all(encoded: &[String], what: &str) -> std::result::Result<Vec<Vec<u8>>, HttpResponse> {
    encoded
        .iter()
        .map(|value| {
            STANDARD.decode(value).map_err(|e| {
                HttpResponse::BadRequest().json(ErrorResponse {
                    error: format!("{what} is not valid base64: {e}"),
                })
            })
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
        AddHostsError::Invalid(_) => {
            tracing::warn!("tenant add-hosts {stage}: refused the request: {error}");
            HttpResponse::BadRequest().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        AddHostsError::Canton(_) => {
            tracing::error!("tenant add-hosts {stage}: Canton call failed: {error}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: error.to_string(),
            })
        }
    }
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
    fn add_hosts_onboard_request_keeps_its_wire_shape() {
        let json = serde_json::json!({
            "party_id": "alice::1220aa",
            "base_serial": 4,
            "topology_transactions": ["AQID"],
            "signatures": ["BAU="],
            "signed_by": "1220aa",
        });
        let Ok(request) = serde_json::from_value::<TenantAddHostsOnboardRequest>(json) else {
            panic!("the documented add-hosts onboard shape must deserialize");
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
}
