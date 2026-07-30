//! Wallet-driven external-party onboarding via the tenant API (`/v0/tenant/*`).
//!
//! Stands in for a wallet: generates an Ed25519 key locally (see
//! [`crate::common::wallet`]), calls `POST /v0/tenant/prepare` on one host to get
//! the multi-host onboarding topology + the multi-hash, signs the multi-hash
//! locally, then submits the same signed bundle to `POST /v0/tenant/onboard` on
//! EACH host itself — DPM never relays between hosts and never sees the private
//! key. Asserts each host reports the party hosted via
//! `GET /v0/tenant/{party}/status` and that the ACS is readable via
//! `GET /v0/tenant/{party}/acs`.
//!
//! (Full transacting — prepare-submission/execute-submission of a real contract
//! — is exercised against DevNet, since it needs a concrete template; here we
//! confirm onboarding + the ACS read end-to-end.)

use std::time::Duration;

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture, chaos::fresh_prefix, http::probe_workflow_status, scenario::Scenario,
    wallet::ExternalKeyPair,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: external_party_tenant");

    // The "wallet": key generated + held client-side; DPM only ever sees the
    // public key and the signature.
    let wallet = ExternalKeyPair::generate();
    let seed = wallet.seed();
    let hint = fresh_prefix("tenant-ext");
    let party_id = wallet.party_id(&hint);
    let public_key = STANDARD.encode(wallet.public_key_bytes());
    info!("Wallet-driven external party: {party_id}");

    Scenario::with_ctx(
        format!("onboard wallet-driven external party {hint} via /v0/tenant/*"),
        (),
    )
    .when(
        "wallet prepares once, signs, and onboards on every host itself",
        {
            let hint = hint.clone();
            let public_key = public_key.clone();
            move |f, _| {
                let hint = hint.clone();
                let public_key = public_key.clone();
                Box::pin(async move {
                    // 1) One host builds the multi-host topology from the wallet's
                    // pubkey (2-of-3 across P1+P2+P3). Threshold is capped at N-1.
                    let prepare_req = json!({
                        "party_hint": hint,
                        "public_key": public_key,
                        "hosting_peers": [&f.p2.participant_id, &f.p3.participant_id],
                        "confirmation_threshold": 2,
                    });
                    let prep: Value = f
                        .post_json(f.p1.http, "/v0/tenant/prepare", &prepare_req)
                        .await?;
                    let multi_hash_b64 = prep
                        .get("multi_hash")
                        .and_then(Value::as_str)
                        .context("prepare response missing multi_hash")?;
                    let multi_hash = STANDARD
                        .decode(multi_hash_b64)
                        .context("multi_hash is not valid base64")?;
                    let topology_transactions = prep
                        .get("topology_transactions")
                        .cloned()
                        .context("prepare response missing topology_transactions")?;

                    // 2) The wallet signs the multi-hash locally with its own key.
                    let wallet = ExternalKeyPair::from_seed(seed);
                    let signature = STANDARD.encode(wallet.sign(&multi_hash));

                    // 3) The wallet submits the SAME signed bundle to every host
                    // itself — no host relays to another. `onboard` carries no host
                    // set / threshold: those live in the signed topology txs.
                    let onboard_req = json!({
                        "party_hint": hint,
                        "public_key": public_key,
                        "topology_transactions": topology_transactions,
                        "multi_hash_signature": signature,
                        "signed_by": wallet.fingerprint(),
                    });
                    for host in [f.p1.http, f.p2.http, f.p3.http] {
                        let _: Value = f
                            .post_json(host, "/v0/tenant/onboard", &onboard_req)
                            .await?;
                    }
                    Ok(())
                })
            }
        },
    )
    .then(
        "party hosted on P1 (topology authorized)",
        Duration::from_secs(180),
        {
            let party_id = party_id.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    probe_workflow_status(
                        &*f,
                        f.p1.http,
                        &format!("/v0/tenant/{party_id}/status"),
                        "tenant-onboarding",
                    )
                    .await
                })
            }
        },
    )
    .then("party hosted on P2", Duration::from_secs(60), {
        let party_id = party_id.clone();
        move |f, _| {
            let party_id = party_id.clone();
            Box::pin(async move {
                probe_workflow_status(
                    &*f,
                    f.p2.http,
                    &format!("/v0/tenant/{party_id}/status"),
                    "tenant-onboarding",
                )
                .await
            })
        }
    })
    .then("party hosted on P3", Duration::from_secs(60), {
        let party_id = party_id.clone();
        move |f, _| {
            let party_id = party_id.clone();
            Box::pin(async move {
                probe_workflow_status(
                    &*f,
                    f.p3.http,
                    &format!("/v0/tenant/{party_id}/status"),
                    "tenant-onboarding",
                )
                .await
            })
        }
    })
    .then(
        "party ACS readable via /v0/tenant/{party}/acs",
        Duration::from_secs(30),
        {
            let party_id = party_id.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    let resp: Value = match f
                        .get_json(f.p1.http, &format!("/v0/tenant/{party_id}/acs"))
                        .await
                    {
                        Ok(r) => r,
                        // Surface a real HTTP error instead of retrying it away.
                        Err(e) => return Some(Err(e)),
                    };
                    // A freshly onboarded party has no contracts yet; the check is
                    // that the endpoint answers with a contracts array.
                    resp.get("contracts")
                        .and_then(Value::as_array)
                        .map(|_| Ok(()))
                })
            }
        },
    )
    .run(f)
    .await
}
