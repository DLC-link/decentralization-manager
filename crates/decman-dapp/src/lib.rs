//! Browser-side checks for the DecMan dApp.
//!
//! The dApp's TypeScript makes every network call and talks to the owners'
//! wallets. This crate runs the checks whose result must match the server's:
//! the party id that a set of owners derives, the digest that each owner
//! signs, and the signature check. It does no I/O, so the native tests run
//! the same code as the browser.
//!
//! The functions marked `#[wasm_bindgen]` are the JavaScript surface. They
//! take and return strings, and they turn errors into JavaScript exceptions.

mod digest;
mod error;
mod party;
mod signature;

use wasm_bindgen::prelude::*;

pub use digest::payload_digest;
pub use error::Error;
pub use party::{PartyPlan, PartyProposal, PlannedOwner, ProposedOwner, plan_party};
pub use signature::{parse_public_key, verify_signature};

/// Checks a party proposal and returns its plan, both as JSON.
#[wasm_bindgen(js_name = planParty)]
pub fn plan_party_js(proposal_json: &str) -> Result<String, JsError> {
    let proposal: PartyProposal = serde_json::from_str(proposal_json)?;
    let plan = plan_party(&proposal)?;
    Ok(serde_json::to_string(&plan)?)
}

/// The digest that owners sign for `payload`, separated by `domain`.
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
