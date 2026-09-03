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
//! After the topology lands the phase drives the rest of the replication: the
//! wallet pulls the ACS from a current host **a range at a time** and relays
//! each to the joiner, which appends until it holds the whole snapshot, then
//! imports it and clears Canton's onboarding marker. Relaying in ranges is what
//! keeps the snapshot out of a single request body, so the loop here is the
//! shape a real wallet uses rather than a test convenience. The final
//! assertion is that P3 reports the party fully hosted — marker gone — which is
//! the only state in which it can actually confirm for the party.
//!
//! That last assertion is also the empirical answer to an open question from the
//! scoping study: whether a single-key party's onboarding participant can clear
//! its own flag, or whether the party key must sign a second round. The proto
//! says the former; decparties were observed to need owner signatures anyway. If
//! this phase's final step times out, the answer is the latter and the wallet
//! needs a signing round.
//!
//! The base serial comes from `GET /v0/tenant/{party}/state`, the way a real
//! wallet learns it, rather than from the test knowing a freshly onboarded party
//! sits at 1.
//!
//! The phase finishes with the threshold raise, which is Plan A's last step and
//! the half of spike question 3 that had never run. It pins both directions: a
//! threshold the live hosts can field is accepted, and one above them is refused
//! before it can be written — the "forbid" half of the plan's fix-or-forbid on
//! the full-threshold bug.

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

/// Bytes per relayed range.
///
/// Deliberately far below the endpoint's 8 MiB default so this party's small ACS
/// still takes several rounds. A range large enough to swallow the whole
/// snapshot would exercise the endpoints while skipping the chunking entirely,
/// which is the part most likely to be wrong.
const RANGE_LIMIT: u64 = 512;

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
                        // The way a real wallet learns it. Asserting it is 1 as
                        // well, because a freshly onboarded party should be, and
                        // a surprise there would mean the endpoint is reporting
                        // something else entirely.
                        let state: Value = f
                            .get_json(f.p1.http, &format!("/v0/tenant/{party_id}/state"))
                            .await?;
                        let base_serial = state
                            .get("serial")
                            .and_then(Value::as_u64)
                            .context("party state missing serial")?;
                        anyhow::ensure!(
                            base_serial == 1,
                            "a freshly onboarded party should sit at serial 1, got {base_serial}"
                        );

                        let request = json!({
                            "party_id": party_id,
                            "new_hosts": [&f.p3.participant_id],
                            "base_serial": base_serial,
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
                            serial == base_serial + 1,
                            "add-hosts must write exactly one serial past the base, got {serial}"
                        );

                        let signatures = sign_prepared(first, &seed)?;

                        // Canton needs the party namespace plus the joining
                        // participant, so P3 alone submits. The current hosts
                        // do not sign an add.
                        let onboard_req = json!({
                            "party_id": party_id,
                            "base_serial": base_serial,
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
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // Scenario 3 — give the new host the contracts, and switch it on.
    // ------------------------------------------------------------------
    Scenario::with_ctx(format!("replicate {hint}'s ACS onto P3"), ())
        .when("wallet relays the snapshot from P1 to P3", {
            let party_id = party_id.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    // Pulled from a host that already holds the party, scoped
                    // to the joiner, and relayed a range at a time. Neither end
                    // ever holds the whole snapshot in a request body.
                    let target = f.p3.participant_id.clone();

                    // The staged replication is keyed by the serial the add was
                    // pinned to, not the current one. The add advanced exactly
                    // one serial, so the base is one behind head state.
                    let state: Value = f
                        .get_json(f.p1.http, &format!("/v0/tenant/{party_id}/state"))
                        .await?;
                    let base_serial = state
                        .get("serial")
                        .and_then(Value::as_u64)
                        .context("party state missing serial")?
                        .checked_sub(1)
                        .context("the add-hosts write should have advanced the serial")?;

                    let mut offset: u64 = 0;
                    let mut rounds = 0;
                    // Set on the first range; the completion assertion below
                    // reads whatever the last round saw.
                    #[allow(unused_assignments)]
                    let mut total_size = 0u64;
                    let mut probed_bad_offset = false;
                    let result = loop {
                        rounds += 1;
                        anyhow::ensure!(rounds < 512, "the relay did not converge");

                        // A deliberately tiny range so this party's small ACS
                        // still needs several of them. Without it the loop
                        // completes on the first pass and the chunking — offset
                        // advancement, staged appends, completion detection —
                        // is never actually exercised.
                        let range: Value = f
                            .get_json(
                                f.p1.http,
                                &format!(
                                    "/v0/tenant/{party_id}/acs/{target}\
                                     ?offset={offset}&limit={RANGE_LIMIT}\
                                     &base_serial={base_serial}"
                                ),
                            )
                            .await?;
                        total_size = range
                            .get("total_size")
                            .and_then(Value::as_u64)
                            .context("acs range missing total_size")?;
                        // Once, mid-transfer: a range at the wrong offset must be
                        // refused rather than written. A hole would only surface
                        // mid-import, with the participant already disconnected.
                        if !probed_bad_offset && offset > 0 {
                            probed_bad_offset = true;
                            let bogus = json!({
                                "party_id": party_id,
                                "base_serial": base_serial,
                                "offset": offset + 7,
                                "total_size": total_size,
                                "chunk": range
                                    .get("chunk")
                                    .cloned()
                                    .context("acs range missing chunk")?,
                                "package_ids": [],
                            });
                            let refused: anyhow::Result<Value> = f
                                .post_json(f.p3.http, "/v0/tenant/add-hosts/import", &bogus)
                                .await;
                            anyhow::ensure!(
                                refused.is_err(),
                                "a range at the wrong offset must be refused, not written"
                            );
                        }

                        let import_req = json!({
                            "party_id": party_id,
                            "base_serial": base_serial,
                            "offset": offset,
                            "total_size": total_size,
                            "chunk": range
                                .get("chunk")
                                .cloned()
                                .context("acs range missing chunk")?,
                            "package_ids": range
                                .get("package_ids")
                                .cloned()
                                .context("acs range missing package_ids")?,
                        });
                        let progress: Value = f
                            .post_json(f.p3.http, "/v0/tenant/add-hosts/import", &import_req)
                            .await?;

                        if progress.get("complete").and_then(Value::as_bool) == Some(true) {
                            break progress;
                        }
                        let received = progress
                            .get("received")
                            .and_then(Value::as_u64)
                            .context("import progress missing received")?;
                        anyhow::ensure!(
                            received > offset,
                            "the joiner did not advance past {offset}"
                        );
                        offset = received;
                    };
                    // The point of the loop: prove it really did take several
                    // ranges, so a green run means the chunking works rather
                    // than that it was skipped.
                    if total_size > RANGE_LIMIT {
                        anyhow::ensure!(
                            rounds > 1,
                            "a {total_size}-byte snapshot at {RANGE_LIMIT}-byte ranges must take \
                             more than one round, took {rounds}"
                        );
                    }
                    info!(
                        "add-hosts import on P3 after {rounds} range(s) of {total_size} byte(s): \
                         {result}"
                    );
                    Ok(())
                })
            }
        })
        // The marker is the difference between "in the mapping" and "usable".
        // A host still carrying it holds none of the party's contracts and
        // cannot confirm, so this is the assertion that the flow actually works.
        .then(
            "P3 hosts the party with the onboarding marker cleared",
            Duration::from_secs(180),
            {
                let party_id = party_id.clone();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        probe_workflow_status(
                            &*f,
                            f.p3.http,
                            &format!("/v0/tenant/{party_id}/status"),
                            "tenant-add-hosts",
                        )
                        .await
                    })
                }
            },
        )
        .run(f)
        .await?;

    // Plan A's last step, and the half of spike question 3 that had never run.
    raise_threshold(f, &party_id, seed).await
}

/// Raise the party's confirmation threshold once its new host is live, and pin
/// the boundary the plan flags as a known bug.
///
/// This is the last step of Plan A and the half of spike question 3 that had
/// never run: the threshold raise is a separate serial bump precisely because a
/// marked host cannot confirm, and the plan warns of a "full-threshold bug"
/// where a write at threshold = host count never becomes effective.
///
/// So it asserts both directions. A threshold the party's live hosts can field
/// is accepted; one above them is refused before it can be written, rather than
/// accepted and left stuck.
async fn raise_threshold(f: &mut Fixture, party_id: &str, seed: [u8; 32]) -> anyhow::Result<()> {
    Scenario::with_ctx(format!("raise {party_id}'s threshold"), ())
        .when("wallet raises the threshold to 2 of 3", {
            let party_id = party_id.to_string();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    let state: Value = f
                        .get_json(f.p1.http, &format!("/v0/tenant/{party_id}/state"))
                        .await?;
                    let base_serial = state
                        .get("serial")
                        .and_then(Value::as_u64)
                        .context("party state missing serial")?;
                    let hosts = state
                        .get("host_count")
                        .and_then(Value::as_u64)
                        .context("party state missing host_count")?;
                    let onboarding = state
                        .get("onboarding_hosts")
                        .and_then(Value::as_u64)
                        .context("party state missing onboarding_hosts")?;
                    anyhow::ensure!(
                        hosts == 3 && onboarding == 0,
                        "the raise needs three live hosts, saw {hosts} with {onboarding} marked"
                    );

                    // The full-threshold case first, because it must fail before
                    // anything is signed. A threshold above what the live hosts
                    // can field is the bug the plan warns about; refusing it here
                    // is the "forbid" half of fix-or-forbid.
                    let too_high = json!({
                        "party_id": party_id,
                        "new_threshold": hosts + 1,
                        "base_serial": base_serial,
                    });
                    let refused: anyhow::Result<Value> = f
                        .post_json(f.p1.http, "/v0/tenant/threshold/prepare", &too_high)
                        .await;
                    // Not merely that it failed — a transport blip or an auth
                    // regression would satisfy that and the test would still
                    // claim the boundary is enforced. Pin the reason.
                    let Err(e) = refused else {
                        anyhow::bail!(
                            "a threshold above the host count must be refused, not written"
                        );
                    };
                    let reason = format!("{e:#}");
                    anyhow::ensure!(
                        reason.contains("400") && reason.contains("able to confirm"),
                        "the refusal must be the threshold bound, got: {reason}"
                    );

                    // Now the real raise: 2 of 3.
                    let request = json!({
                        "party_id": party_id,
                        "new_threshold": 2,
                        "base_serial": base_serial,
                    });
                    let prep: Value = f
                        .post_json(f.p1.http, "/v0/tenant/threshold/prepare", &request)
                        .await?;
                    let signatures = sign_prepared(&prep, &seed)?;
                    let onboard = json!({
                        "party_id": party_id,
                        "base_serial": base_serial,
                        "topology_transactions": prep
                            .get("topology_transactions")
                            .cloned()
                            .context("threshold prepare missing topology_transactions")?,
                        "signatures": signatures,
                        "signed_by": ExternalKeyPair::from_seed(seed).fingerprint(),
                    });
                    // A threshold change needs the party namespace alone, so one
                    // host carries it.
                    let _: Value = f
                        .post_json(f.p1.http, "/v0/tenant/threshold/onboard", &onboard)
                        .await?;
                    Ok(())
                })
            }
        })
        .then(
            "the party reports threshold 2 of 3",
            Duration::from_secs(180),
            {
                let party_id = party_id.to_string();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        let state: Value = f
                            .probe_get_json(f.p1.http, &format!("/v0/tenant/{party_id}/state"))
                            .await?;
                        let threshold = state.get("threshold").and_then(Value::as_u64)?;
                        let hosts = state.get("host_count").and_then(Value::as_u64)?;
                        if threshold != 2 || hosts != 3 {
                            return None;
                        }
                        info!("threshold raised to {threshold} of {hosts}");
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
