//! Wallet-driven external-party onboarding via the tenant API (`/v0/tenant/*`).
//!
//! Stands in for a wallet: generates an Ed25519 key locally with the shipped
//! wallet library ([`decman_wallet::ExternalKeyPair`] — the same type a wallet
//! provider uses, so there is one implementation of the key handling and the
//! fingerprint derivation), calls `POST /v0/tenant/prepare` on one host to get
//! the multi-host onboarding topology + one hash per transaction, signs each hash
//! locally, then submits the same signed bundle to `POST /v0/tenant/onboard` on
//! EACH host itself — DPM never relays between hosts and never sees the private
//! key. Asserts each host reports the party hosted via
//! `GET /v0/tenant/{party}/status`.
//!
//! Transacting as the party is not part of the tenant API — a wallet does that
//! directly against Canton with the key it holds — so this phase covers
//! onboarding only.

use std::time::Duration;

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tracing::info;

use decman_wallet::ExternalKeyPair;

use crate::common::{
    Fixture, chaos::fresh_prefix, http::probe_workflow_status, scenario::Scenario,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: external_party_tenant");

    // The "wallet": key generated + held client-side; DPM only ever sees the
    // public key and the signature.
    let wallet = ExternalKeyPair::generate();
    // Copied out of its `Zeroizing` wrapper so the closure below can rebuild the
    // key; the wrapper itself is not `Copy`.
    let seed = *wallet.seed();
    let hint = fresh_prefix("tenant-ext");
    let party_id = wallet.party_id(&hint);
    let public_key = wallet.public_key_b64();
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
                    let hashes = prep
                        .get("transaction_hashes")
                        .and_then(Value::as_array)
                        .context("prepare response missing transaction_hashes")?
                        .iter()
                        .map(|h| {
                            let encoded = h
                                .as_str()
                                .context("transaction_hashes entry is not a string")?;
                            STANDARD
                                .decode(encoded)
                                .context("transaction hash is not valid base64")
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    let topology_transactions = prep
                        .get("topology_transactions")
                        .cloned()
                        .context("prepare response missing topology_transactions")?;

                    // 2) The wallet signs each transaction hash locally with its own key.
                    let wallet = ExternalKeyPair::from_seed(seed);
                    // One signature per transaction, over the hash Canton returned
                    // for it.
                    let signatures: Vec<String> = hashes
                        .iter()
                        .map(|h| STANDARD.encode(wallet.sign(h)))
                        .collect();

                    // 3) The wallet submits the SAME signed bundle to every host
                    // itself — no host relays to another. `onboard` carries no host
                    // set / threshold: those live in the signed topology txs.
                    let onboard_req = json!({
                        "party_hint": hint,
                        "public_key": public_key,
                        "topology_transactions": topology_transactions,
                        "signatures": signatures,
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
    .run(f)
    .await
}
