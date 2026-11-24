//! Browser-side Rust for the DecMan dApp.
//!
//! The dApp's TypeScript makes every network call and talks to the owners'
//! wallets. This crate computes what must match the server and Canton: the
//! party id that a set of owners derives, the topology transactions that
//! create the party, the hash every owner signs, the signature checks, and
//! the JSON Ledger API bodies. It does no I/O, so the native tests run the
//! same code as the browser.
//!
//! The functions marked `#[wasm_bindgen]` are the JavaScript surface. They
//! take and return strings, and they turn errors into JavaScript exceptions.

mod digest;
mod error;
mod onboarding;
mod party;
mod signature;

use serde::de::DeserializeOwned;
use wasm_bindgen::prelude::*;

pub use digest::payload_digest;
pub use error::Error;
pub use onboarding::{
    AllocationRequests, Onboarding, OnboardingTransaction, OwnerSignature, allocation_requests,
    check_owner_signatures, generate_topology_request, onboarding_transactions,
    party_hosted_locally, topology_format, topology_format_of_generated,
};
pub use party::{MAX_OWNERS, PartyPlan, PartyProposal, PlannedOwner, ProposedOwner, plan_party};
pub use signature::{parse_public_key, verify_signature};

fn parse<T: DeserializeOwned>(json: &str) -> Result<T, Error> {
    Ok(serde_json::from_str(json)?)
}

/// Checks a party proposal and returns its plan, both as JSON.
#[wasm_bindgen(js_name = planParty)]
pub fn plan_party_js(proposal_json: &str) -> Result<String, JsError> {
    let plan = plan_party(&parse(proposal_json)?)?;
    Ok(serde_json::to_string(&plan)?)
}

/// The onboarding transactions and the hash every owner signs, as JSON.
#[wasm_bindgen(js_name = onboardingTransactions)]
pub fn onboarding_transactions_js(
    proposal_json: &str,
    topology_version: u32,
) -> Result<String, JsError> {
    let plan = plan_party(&parse(proposal_json)?)?;
    let onboarding = onboarding_transactions(&plan, topology_format(topology_version)?);
    Ok(serde_json::to_string(&onboarding)?)
}

/// The two `AllocateExternalParty` bodies, as JSON. Throws unless every owner
/// signed the combined hash.
#[wasm_bindgen(js_name = allocationRequests)]
pub fn allocation_requests_js(
    proposal_json: &str,
    topology_version: u32,
    synchronizer_id: &str,
    signatures_json: &str,
) -> Result<String, JsError> {
    let plan = plan_party(&parse(proposal_json)?)?;
    let onboarding = onboarding_transactions(&plan, topology_format(topology_version)?);
    let signatures: Vec<OwnerSignature> = parse(signatures_json)?;
    let requests = allocation_requests(&plan, &onboarding, synchronizer_id, &signatures)?;
    Ok(serde_json::to_string(&requests)?)
}

/// The generate-topology body that probes a participant's topology format.
#[wasm_bindgen(js_name = generateTopologyRequest)]
pub fn generate_topology_request_js(
    synchronizer_id: &str,
    public_key_hex: &str,
) -> Result<String, JsError> {
    let request = generate_topology_request(synchronizer_id, public_key_hex)?;
    Ok(serde_json::to_string(&request)?)
}

/// The topology format (30 or 31) of a generate-topology answer.
#[wasm_bindgen(js_name = topologyVersionOfGenerated)]
pub fn topology_version_of_generated_js(response_json: &str) -> Result<u32, JsError> {
    let format = topology_format_of_generated(&parse(response_json)?)?;
    Ok(format.proto_version().unsigned_abs())
}

/// Whether a get-party answer says the participant hosts `party_id`.
#[wasm_bindgen(js_name = partyHostedLocally)]
pub fn party_hosted_locally_js(response_json: &str, party_id: &str) -> Result<bool, JsError> {
    Ok(party_hosted_locally(&parse(response_json)?, party_id))
}

/// The digest that owners sign for `payload`, separated by `domain`. Only
/// the simulated transfer requests use it.
#[wasm_bindgen(js_name = payloadDigest)]
pub fn payload_digest_js(domain: &str, payload: &str) -> String {
    payload_digest(domain, payload)
}

/// The Canton namespace of a hex-encoded raw Ed25519 public key.
#[wasm_bindgen(js_name = publicKeyNamespace)]
pub fn public_key_namespace_js(public_key_hex: &str) -> Result<String, JsError> {
    let key = parse_public_key(public_key_hex)?;
    Ok(common::fingerprint::fingerprint_from_public_key(key.as_bytes()).to_string())
}

/// Whether `signature_hex` is a valid Ed25519 signature of `digest_hex`.
#[wasm_bindgen(js_name = verifySignature)]
pub fn verify_signature_js(
    public_key_hex: &str,
    digest_hex: &str,
    signature_hex: &str,
) -> Result<bool, JsError> {
    Ok(verify_signature(public_key_hex, digest_hex, signature_hex)?)
}
