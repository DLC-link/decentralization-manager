//! Adding a host to an **existing** external party via `/v0/tenant/add-hosts/*`.
//!
//! The tenant API's onboarding path writes serial 1 and creates a party. This
//! phase covers the other write: serial N+1 against a party that already
//! exists, which is what lets an external party gain a host after creation.
//!
//! The party is onboarded on P1+P2 only, so P3 is a genuine new host rather
//! than one that was there all along.
//!
//! The property under test is the one the whole design rests on: **every host
//! independently builds byte-identical bytes**. That is what lets the wallet
//! compare them before it signs, and it is the only thing standing between the
//! wallet and a lying host. A phase that prepared on one host would not test it.
//!
//! ## What this does NOT yet cover
//!
//! The flow currently stops at the topology write. Nothing replicates the ACS
//! onto the new host and nothing clears Canton's onboarding marker, so P3 ends
//! up named in the mapping while the party is still suspended there and holds
//! none of its contracts. This phase therefore asserts the mapping advanced —
//! not that the new host is usable.
//!
//! Two known gaps show up here and are deliberately not asserted as correct:
//!
//! * The wallet has no endpoint that reports a party's current serial, so the
//!   base serial is pinned from knowledge that a freshly onboarded party sits at
//!   1. A real wallet cannot do that.
//! * `host_onboarding_status` decides "hosted" without looking at the
//!   onboarding marker, so `/v0/tenant/{party}/status` would call P3 hosted the
//!   moment the mapping is authorized — before the party is usable there.
//!
//! Both are tracked as the next piece of work; this phase should tighten to a
//! real end-to-end assertion once the ACS replication and the flag-clear land.

use std::time::Duration;

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

use decman_wallet::ExternalKeyPair;

use crate::common::{
    Fixture, chaos::fresh_prefix, http::probe_workflow_status, scenario::Scenario,
};

/// A freshly onboarded party's `PartyToParticipant` sits at serial 1. Pinned
/// here because no endpoint reports it yet (see the module docs).
const BASE_SERIAL: u32 = 1;

/// Read side of `/external-parties`. The shipped `ExternalPartiesResponse` is
/// serialize-only, so the test declares the fields it reads.
#[derive(Debug, Deserialize)]
struct ListedParties {
    parties: Vec<ListedParty>,
}

#[derive(Debug, Deserialize)]
struct ListedParty {
    party_id: String,
    host_count: u32,
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: external_party_add_hosts");

    let wallet = ExternalKeyPair::generate();
    let seed = *wallet.seed();
    let hint = fresh_prefix("tenant-addhost");
    let party_id = wallet.party_id(&hint);
    let public_key = wallet.public_key_b64();
    info!("External party for add-hosts: {party_id}");

    // ------------------------------------------------------------------
    // Scenario 1 — a two-host party to grow.
    // ------------------------------------------------------------------
    Scenario::with_ctx(format!("onboard {hint} on P1 and P2 only"), ())
        .when("wallet onboards the party across two hosts", {
            let hint = hint.clone();
            let public_key = public_key.clone();
            move |f, _| {
                let hint = hint.clone();
                let public_key = public_key.clone();
                Box::pin(async move {
                    // Threshold 1: the cap is N-1, and N is 2 here.
                    let prepare_req = json!({
                        "party_hint": hint,
                        "public_key": public_key,
                        "hosting_peers": [&f.p2.participant_id],
                        "confirmation_threshold": 1,
                    });
                    let prep: Value = f
                        .post_json(f.p1.http, "/v0/tenant/prepare", &prepare_req)
                        .await?;
                    let signatures = sign_prepared(&prep, &seed)?;
                    let onboard_req = json!({
                        "party_hint": hint,
                        "public_key": public_key,
                        "topology_transactions": prep
                            .get("topology_transactions")
                            .cloned()
                            .context("prepare response missing topology_transactions")?,
                        "signatures": signatures,
                        "signed_by": ExternalKeyPair::from_seed(seed).fingerprint(),
                    });
                    for host in [f.p1.http, f.p2.http] {
                        let _: Value = f
                            .post_json(host, "/v0/tenant/onboard", &onboard_req)
                            .await?;
                    }
                    Ok(())
                })
            }
        })
        .then("party hosted on P1", Duration::from_secs(180), {
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
        })
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
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // Scenario 2 — grow it to three.
    // ------------------------------------------------------------------
    Scenario::with_ctx(format!("add P3 as a host of {hint}"), ())
        .when(
            "every host prepares the same serial-2 bytes, and the new host submits",
            {
                let party_id = party_id.clone();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        let request = json!({
                            "party_id": party_id,
                            "new_hosts": [&f.p3.participant_id],
                            "base_serial": BASE_SERIAL,
                        });

                        // Every host — the two current ones AND the joiner —
                        // builds the replace independently. The joiner can read
                        // the mapping because it lives in the shared
                        // synchronizer store, not in per-host state.
                        let mut prepared = Vec::new();
                        for host in [f.p1.http, f.p2.http, f.p3.http] {
                            let prep: Value = f
                                .post_json(host, "/v0/tenant/add-hosts/prepare", &request)
                                .await?;
                            prepared.push(prep);
                        }

                        // THE property: the wallet can only detect a lying host
                        // by comparing what the others produced, so identical
                        // bytes are load-bearing, not incidental.
                        let first = &prepared[0];
                        for (index, other) in prepared.iter().enumerate().skip(1) {
                            anyhow::ensure!(
                                first.get("topology_transactions")
                                    == other.get("topology_transactions"),
                                "host {index} prepared different bytes from host 0 — the wallet \
                                 could not compare them:\n  host 0: {first:?}\n  host {index}: \
                                 {other:?}"
                            );
                        }

                        let serial = first
                            .get("serial")
                            .and_then(Value::as_u64)
                            .context("prepare response missing serial")?;
                        anyhow::ensure!(
                            serial == u64::from(BASE_SERIAL) + 1,
                            "add-hosts must write exactly one serial past the base, got {serial}"
                        );

                        let signatures = sign_prepared(first, &seed)?;

                        // Canton needs the party namespace plus the joining
                        // participant, so P3 alone submits. The current hosts
                        // do not sign an add.
                        let onboard_req = json!({
                            "party_id": party_id,
                            "base_serial": BASE_SERIAL,
                            "topology_transactions": first
                                .get("topology_transactions")
                                .cloned()
                                .context("prepare response missing topology_transactions")?,
                            "signatures": signatures,
                            "signed_by": ExternalKeyPair::from_seed(seed).fingerprint(),
                        });
                        let _: Value = f
                            .post_json(f.p3.http, "/v0/tenant/add-hosts/onboard", &onboard_req)
                            .await?;
                        Ok(())
                    })
                }
            },
        )
        // Reads the authorized mapping rather than any status endpoint: a host
        // count of 3 is only true once the serial-2 write is live.
        .then(
            "P1's authorized mapping names three hosts",
            Duration::from_secs(180),
            {
                let party_id = party_id.clone();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        let listed: ListedParties =
                            f.probe_get_json(f.p1.http, "/external-parties").await?;
                        let party = listed.parties.iter().find(|p| p.party_id == party_id)?;
                        if party.host_count != 3 {
                            return None;
                        }
                        Some(Ok(()))
                    })
                }
            },
        )
        .then(
            "the joiner sees itself hosting the party",
            Duration::from_secs(120),
            {
                let party_id = party_id.clone();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        let listed: ListedParties =
                            f.probe_get_json(f.p3.http, "/external-parties").await?;
                        listed.parties.iter().find(|p| p.party_id == party_id)?;
                        Some(Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await
}

/// Sign every transaction hash in a prepare response with the wallet's key,
/// returning the base64 signatures index-aligned with the transactions.
fn sign_prepared(prepared: &Value, seed: &[u8; 32]) -> anyhow::Result<Vec<String>> {
    let wallet = ExternalKeyPair::from_seed(*seed);
    prepared
        .get("transaction_hashes")
        .and_then(Value::as_array)
        .context("prepare response missing transaction_hashes")?
        .iter()
        .map(|hash| {
            let encoded = hash
                .as_str()
                .context("transaction_hashes entry is not a string")?;
            let bytes = STANDARD
                .decode(encoded)
                .context("transaction hash is not valid base64")?;
            Ok(STANDARD.encode(wallet.sign(&bytes)))
        })
        .collect()
}
