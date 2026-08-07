//! The demo wallet's local HTTP API, driven by its own UI.
//!
//! Reads and writes walk the hosting set in order and use the first host that
//! answers. That is not resilience theatre — it is the property co-validation
//! buys: any one host can be down and the party keeps working. The response says
//! which host served it, so the UI can show that happening.

use actix_web::{HttpResponse, Responder, delete, get, http::StatusCode, post, web};
use common::api::{TenantContract, TenantTemplateId};
use serde::{Deserialize, Serialize};

use crate::{
    Error, ExternalKeyPair, create_contract, demo::DemoState, flow::HostReport,
    onboard_co_validated, statuses,
};

/// The party this wallet holds, as the UI shows it. No secret is included: the
/// seed never leaves the wallet process.
#[derive(Debug, Serialize)]
pub struct PartyView {
    pub party_id: String,
    pub party_hint: String,
    pub fingerprint: String,
    /// Base64 public key — the only half of the key any host ever sees.
    pub public_key: String,
}

#[derive(Debug, Serialize)]
pub struct ConfigView {
    pub hosts: Vec<super::HostView>,
    pub confirmation_threshold: Option<u32>,
    /// The party in play, if this wallet already holds one.
    pub party: Option<PartyView>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePartyRequest {
    pub party_hint: String,
    /// Overrides the configured default. `None` lets DecMan pick `N-1`.
    #[serde(default)]
    pub confirmation_threshold: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct CreateContractRequest {
    pub template_id: TenantTemplateId,
    pub create_arguments: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct StatusView {
    pub party_id: String,
    pub hosts: Vec<HostReport>,
    /// True once every host has authorized the party.
    pub fully_hosted: bool,
}

#[derive(Debug, Serialize)]
pub struct AcsView {
    pub contracts: Vec<TenantContract>,
    /// Which host answered the read.
    pub served_by: String,
}

fn error(status: StatusCode, message: impl std::fmt::Display) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({ "error": message.to_string() }))
}

fn no_party() -> HttpResponse {
    error(
        StatusCode::NOT_FOUND,
        "this wallet holds no party yet — create one first",
    )
}

fn view(key: &ExternalKeyPair, party_hint: &str, party_id: &str) -> PartyView {
    PartyView {
        party_id: party_id.to_string(),
        party_hint: party_hint.to_string(),
        fingerprint: key.fingerprint(),
        public_key: key.public_key_b64(),
    }
}

/// The configured hosting set plus whatever party this wallet already holds.
#[get("/api/config")]
pub async fn config(state: web::Data<DemoState>) -> impl Responder {
    HttpResponse::Ok().json(ConfigView {
        hosts: state.host_views().to_vec(),
        confirmation_threshold: state.confirmation_threshold(),
        party: state
            .current()
            .map(|(key, hint, party_id)| view(&key, &hint, &party_id)),
    })
}

/// Generate a key and onboard a co-validated party across every host.
#[post("/api/party")]
pub async fn create_party(
    state: web::Data<DemoState>,
    body: web::Json<CreatePartyRequest>,
) -> impl Responder {
    let party_hint = body.party_hint.trim().to_string();
    if party_hint.is_empty() {
        return error(StatusCode::BAD_REQUEST, "party_hint must not be empty");
    }
    if state.current().is_some() {
        return error(
            StatusCode::CONFLICT,
            "this wallet already holds a party — reset it to run the demo again",
        );
    }

    // The key is born here, in the wallet, and never leaves.
    let key = ExternalKeyPair::generate();
    let threshold = body
        .confirmation_threshold
        .or_else(|| state.confirmation_threshold());

    match onboard_co_validated(state.hosts(), &key, &party_hint, threshold).await {
        Ok(onboarded) => {
            state.store(&key, &party_hint, &onboarded.party_id);
            HttpResponse::Ok().json(onboarded)
        }
        Err(e) => {
            tracing::error!("onboarding failed: {e}");
            // A host's status must not be echoed verbatim. Relaying its 401 would
            // read in the browser as this wallet rejecting the page, when what
            // actually failed is the wallet-to-host hop. Only a 400 is passed on,
            // since that reflects input the page supplied; anything else upstream
            // is a gateway failure. The message always names the host either way.
            let status = match &e {
                Error::NotEnoughHosts(_) | Error::Api { status: 400, .. } => {
                    StatusCode::BAD_REQUEST
                }
                _ => StatusCode::BAD_GATEWAY,
            };
            error(status, e)
        }
    }
}

/// The party this wallet holds.
#[get("/api/party")]
pub async fn party(state: web::Data<DemoState>) -> impl Responder {
    match state.current() {
        Some((key, hint, party_id)) => HttpResponse::Ok().json(view(&key, &hint, &party_id)),
        None => no_party(),
    }
}

/// Every host's current view of the party. The UI polls this while the topology
/// is being authorized.
#[get("/api/party/status")]
pub async fn party_status(state: web::Data<DemoState>) -> impl Responder {
    let Some((_, _, party_id)) = state.current() else {
        return no_party();
    };
    let hosts = statuses(state.hosts(), &party_id).await;
    HttpResponse::Ok().json(StatusView {
        fully_hosted: !hosts.is_empty() && hosts.iter().all(HostReport::is_hosted),
        party_id,
        hosts,
    })
}

/// The party's active contracts, read from the first host that answers.
#[get("/api/party/acs")]
pub async fn party_acs(state: web::Data<DemoState>) -> impl Responder {
    let Some((_, _, party_id)) = state.current() else {
        return no_party();
    };

    let mut last_error = None;
    for host in state.hosts() {
        match host.client.acs(&party_id).await {
            Ok(contracts) => {
                return HttpResponse::Ok().json(AcsView {
                    contracts,
                    served_by: host.client.base_url().to_string(),
                });
            }
            Err(e) => {
                tracing::warn!(host = host.client.base_url(), "ACS read failed: {e}");
                last_error = Some(e);
            }
        }
    }
    match last_error {
        Some(e) => error(
            StatusCode::BAD_GATEWAY,
            format!("no host served the ACS: {e}"),
        ),
        None => error(StatusCode::BAD_GATEWAY, "no hosts are configured"),
    }
}

/// Create a contract as the party: prepared on a host, signed here, executed there.
#[post("/api/party/contracts")]
pub async fn create_party_contract(
    state: web::Data<DemoState>,
    body: web::Json<CreateContractRequest>,
) -> impl Responder {
    let Some((key, _, party_id)) = state.current() else {
        return no_party();
    };
    let body = body.into_inner();

    let mut last_error = None;
    for host in state.hosts() {
        match create_contract(
            &host.client,
            &key,
            &party_id,
            body.template_id.clone(),
            body.create_arguments.clone(),
        )
        .await
        {
            Ok(()) => {
                return HttpResponse::Ok().json(serde_json::json!({
                    "served_by": host.client.base_url(),
                }));
            }
            Err(e) => {
                tracing::warn!(host = host.client.base_url(), "create failed: {e}");
                last_error = Some(e);
            }
        }
    }
    match last_error {
        Some(e) => error(
            StatusCode::BAD_GATEWAY,
            format!("no host accepted the submission: {e}"),
        ),
        None => error(StatusCode::BAD_GATEWAY, "no hosts are configured"),
    }
}

/// The party's private key, for the wallet's own UI to display.
///
/// This is the actual signing key. It is served only over the loopback address this
/// process binds, and it is never sent to a DecMan host — the point of showing it is
/// that the owner holds it and the hosts do not. A real wallet would put this behind
/// a device unlock; a demo on a throwaway devnet key does not need to pretend.
#[get("/api/party/secret")]
pub async fn party_secret(state: web::Data<DemoState>) -> impl Responder {
    match state.seed_b64() {
        Some(seed) => HttpResponse::Ok().json(serde_json::json!({
            "seed": seed.as_str(),
        })),
        None => no_party(),
    }
}

/// Forget the party and its key, so the demo can be run again from scratch.
#[delete("/api/party")]
pub async fn reset(state: web::Data<DemoState>) -> impl Responder {
    state.clear();
    HttpResponse::NoContent().finish()
}
