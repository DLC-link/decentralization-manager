//! A host leaving an external party on its own, through `POST /v0/tenant/unhost`.
//!
//! The phase onboards a fresh party on P1+P2+P3 at 2 of 3, and then P3 removes
//! itself with its own key. No party signature is involved: Canton accepts a
//! write that keeps the threshold, adds no host and removes one, on the removed
//! host's namespace alone.
//!
//! It then checks what the issue's acceptance asks for:
//!
//! * P1's mapping lists two hosts at threshold 2.
//! * P3 answers 404 for the party's status.
//! * The party still transacts, through P1, with its own key.
//! * P3's audit row reads `completed`, with the Splice cleanup off.
//! * P2 cannot leave as well, because one host cannot meet a threshold of 2.
//!
//! A second party, hosted by P1 and P2 at 1 of 2, covers a host that still
//! holds the party's contracts. Canton refuses that departure unless the write
//! carries the active-contracts force flag.
//!
//! The localnet carries no Splice packages, so every request sends
//! `splice_cleanup: false`. The Splice half is verified on DevNet.
//!
//! `orphan-marker` goes to P1 and P2 only. P3 must stay without it for
//! [`add_party_missing_dar`](super::add_party_missing_dar).

use std::{path::Path, time::Duration};

use anyhow::Context;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tracing::info;

use decman_wallet::ExternalKeyPair;

use crate::common::{
    Fixture, TestTarget,
    chaos::fresh_prefix,
    http::probe_workflow_status,
    ledger_api::P1_JSON_API,
    phases::{
        deploy_gov_core::grant_rights,
        external_party_tenant::{onboard_on_hosts, onboard_on_three_hosts},
        local_party_decentralization::{ORPHAN_DAR, create_marker_as_converted, node_config},
    },
    scenario::Scenario,
};

/// How many times the phase repeats an unhost that is still in flight. A repeat
/// after the write takes effect answers 200.
const UNHOST_ATTEMPTS: usize = 30;

/// This host's serial for the party, from the endpoint a wallet reads it from.
async fn serial_on(f: &Fixture, port: u16, party_id: &str) -> anyhow::Result<u64> {
    let state: Value = f
        .get_json(port, &format!("/v0/tenant/{party_id}/state"))
        .await?;
    state
        .get("serial")
        .and_then(Value::as_u64)
        .context("party state missing serial")
}

fn unhost_request(party_id: &str, participant_id: &str, base_serial: u64) -> Value {
    json!({
        "party_id": party_id,
        "participant_id": participant_id,
        "base_serial": base_serial,
        "splice_cleanup": false,
    })
}

/// A decentralized party's hosts change only through its governance. A tenant
/// key must not make a host leave one, even though Canton would accept the
/// host's own signature. Runs after `create_dec_party`, which leaves the
/// fixture's party hosted by P1, P2 and P3.
pub async fn refuses_a_decentralized_party(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: external_party_unhost (decentralized party)");
    let party_id = f.party_id()?.to_string();

    Scenario::with_ctx(
        format!("P3 refuses to leave decentralized party {party_id}"),
        (),
    )
    .when("a tenant asks P3 to leave it", {
        move |f, _| {
            let party_id = party_id.clone();
            Box::pin(async move {
                let base_serial = serial_on(&*f, f.p3.http, &party_id).await?;
                let (status, body) = f
                    .post_expect_status(
                        f.p3.http,
                        "/v0/tenant/unhost",
                        &unhost_request(&party_id, &f.p3.participant_id, base_serial),
                    )
                    .await?;
                anyhow::ensure!(
                    status == StatusCode::BAD_REQUEST && body.contains("decentralized party"),
                    "a tenant unhost of a decentralized party answered {status}: {body}"
                );
                let after = serial_on(&*f, f.p3.http, &party_id).await?;
                anyhow::ensure!(
                    after == base_serial,
                    "the refused unhost moved the serial from {base_serial} to {after}"
                );
                Ok(())
            })
        }
    })
    .run(f)
    .await
}

/// Ask the host on `port` to leave the party, and repeat while the departure is
/// still in flight. Answers 200 once the host is gone.
async fn leave(
    f: &Fixture,
    port: u16,
    name: &str,
    party_id: &str,
    participant_id: &str,
) -> anyhow::Result<()> {
    let base_serial = serial_on(f, port, party_id).await?;
    let request = unhost_request(party_id, participant_id, base_serial);
    for attempt in 1..=UNHOST_ATTEMPTS {
        let (status, body) = f
            .post_expect_status(port, "/v0/tenant/unhost", &request)
            .await?;
        match status {
            StatusCode::OK => {
                info!("{name} left {party_id} after {attempt} call(s): {body}");
                return Ok(());
            }
            // Accepted but not effective yet. The host keeps the party held
            // until it is, so a repeat answers 409 "already running" until
            // then, and 200 after.
            StatusCode::ACCEPTED => tokio::time::sleep(Duration::from_secs(2)).await,
            StatusCode::CONFLICT if body.contains("already running") => {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            _ => anyhow::bail!("{name}'s unhost answered {status}: {body}"),
        }
    }
    anyhow::bail!("{name}'s unhost never took effect in {UNHOST_ATTEMPTS} calls")
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: external_party_unhost");

    let wallet = ExternalKeyPair::generate();
    let seed = *wallet.seed();
    let hint = fresh_prefix("tenant-unhost");
    let party_id = wallet.party_id(&hint);
    info!("External party P3 will leave: {party_id}");

    // ------------------------------------------------------------------
    // 1 — a party hosted by all three nodes at 2 of 3.
    // ------------------------------------------------------------------
    let mut onboarded = Scenario::with_ctx(format!("{hint} is hosted by P1, P2 and P3"), ()).when(
        "the wallet onboards it on all three hosts at 2 of 3",
        {
            let hint = hint.clone();
            move |f, _| {
                let hint = hint.clone();
                Box::pin(async move { onboard_on_three_hosts(&*f, &hint, seed).await })
            }
        },
    );
    for (name, port) in [("P1", f.p1.http), ("P2", f.p2.http), ("P3", f.p3.http)] {
        let party_id = party_id.clone();
        onboarded = onboarded.then(
            format!("{name} hosts the party"),
            Duration::from_secs(180),
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    probe_workflow_status(
                        &*f,
                        port,
                        &format!("/v0/tenant/{party_id}/status"),
                        "tenant-onboarding",
                    )
                    .await
                })
            },
        );
    }
    onboarded.run(f).await?;

    // ------------------------------------------------------------------
    // 2 — P3 leaves on its own key.
    // ------------------------------------------------------------------
    Scenario::with_ctx(format!("P3 leaves {hint} through /v0/tenant/unhost"), ())
        .when("P3 refuses the requests it must refuse, then leaves", {
            let party_id = party_id.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    let base_serial = serial_on(&*f, f.p3.http, &party_id).await?;

                    // A host removes only itself.
                    let (status, body) = f
                        .post_expect_status(
                            f.p3.http,
                            "/v0/tenant/unhost",
                            &unhost_request(&party_id, &f.p1.participant_id, base_serial),
                        )
                        .await?;
                    anyhow::ensure!(
                        status == StatusCode::BAD_REQUEST,
                        "P3 asked to remove P1 answered {status}: {body}"
                    );

                    // The localnet has no validator operator party, so the
                    // cleanup can only be skipped on purpose. A DevNet node
                    // may have one, and this request would then unhost.
                    if matches!(f.target, TestTarget::Localnet) {
                        let mut with_cleanup =
                            unhost_request(&party_id, &f.p3.participant_id, base_serial);
                        with_cleanup["splice_cleanup"] = Value::Null;
                        let (status, body) = f
                            .post_expect_status(f.p3.http, "/v0/tenant/unhost", &with_cleanup)
                            .await?;
                        anyhow::ensure!(
                            status == StatusCode::UNPROCESSABLE_ENTITY,
                            "an unhost with the Splice cleanup on a node without an operator \
                             party answered {status}: {body}"
                        );
                    }

                    leave(&*f, f.p3.http, "P3", &party_id, &f.p3.participant_id).await
                })
            }
        })
        .then(
            "P1's mapping lists two hosts at threshold 2",
            Duration::from_secs(120),
            {
                let party_id = party_id.clone();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        let state: Value = f
                            .probe_get_json(f.p1.http, &format!("/v0/tenant/{party_id}/state"))
                            .await?;
                        let hosts = state.get("host_count").and_then(Value::as_u64)?;
                        let threshold = state.get("threshold").and_then(Value::as_u64)?;
                        (hosts == 2 && threshold == 2).then_some(Ok(()))
                    })
                }
            },
        )
        .then("P3 no longer hosts the party", Duration::from_secs(60), {
            let party_id = party_id.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    let status = f
                        .get_status(f.p3.http, &format!("/v0/tenant/{party_id}/status"))
                        .await
                        .ok()?;
                    (status == StatusCode::NOT_FOUND).then_some(Ok(()))
                })
            }
        })
        .then(
            "P3's audit row reads completed, with the Splice cleanup off",
            Duration::from_secs(60),
            {
                let party_id = party_id.clone();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        let audit: Value = f
                            .probe_get_json(
                                f.p3.http,
                                &format!("/external-parties/{party_id}/unhost-audit"),
                            )
                            .await?;
                        let newest = audit.get("entries")?.as_array()?.first()?.clone();
                        if newest.get("status")?.as_str()? != "completed" {
                            return None;
                        }
                        let expect = |field: &str, want: Value| -> anyhow::Result<()> {
                            let got = newest.get(field).cloned().unwrap_or(Value::Null);
                            anyhow::ensure!(
                                got == want,
                                "audit {field} is {got}, expected {want}: {newest}"
                            );
                            Ok(())
                        };
                        Some(
                            expect("splice_cleanup", json!(false))
                                .and_then(|()| expect("requested_via", json!("tenant")))
                                .and_then(|()| expect("step", json!("done")))
                                .and_then(|()| expect("validator_rights_archived", json!([])))
                                .and_then(|()| expect("preapprovals_cancelled", json!([]))),
                        )
                    })
                }
            },
        )
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // 3 — the party still transacts through the hosts that remain.
    // ------------------------------------------------------------------
    let p1_config = node_config(&f.p1.participant_id, "P1_CANTON_ADMIN", "P1_CANTON_LEDGER")?;
    let p1_config_held = p1_config.clone();
    let dar_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../releases/v1")
        .join(ORPHAN_DAR);
    let dar_bytes = tokio::fs::read(&dar_path)
        .await
        .with_context(|| format!("reading {path}", path = dar_path.display()))?;
    let upload_req = json!({
        "dar_files": [{ "filename": ORPHAN_DAR, "data": STANDARD.encode(&dar_bytes) }],
    });

    Scenario::with_ctx(format!("{hint} transacts without P3"), ())
        .when(
            "the party creates a contract through P1 with its own key",
            {
                let party_id = party_id.clone();
                move |f, _| {
                    let party_id = party_id.clone();
                    let upload_req = upload_req.clone();
                    let config = p1_config.clone();
                    let wallet = ExternalKeyPair::from_seed(seed);
                    Box::pin(async move {
                        // Both remaining hosts confirm, so both must have vetted
                        // the package. P3 is not a host any more and needs nothing.
                        for (host, name) in [(f.p1.http, "P1"), (f.p2.http, "P2")] {
                            let _: Value = f
                                .post_json(host, "/dars/upload", &upload_req)
                                .await
                                .with_context(|| format!("upload orphan-marker to {name}"))?;
                        }
                        grant_rights(&*f, P1_JSON_API, &party_id, "P1").await?;

                        let token = f.refresher.token().await?;
                        let created =
                            create_marker_as_converted(&config, &token, &party_id, &wallet, 0)
                                .await
                                .context("the party submitting through P1 after P3 left")?;
                        anyhow::ensure!(
                            created == 1,
                            "the submission after P3 left created {created} contract(s), not 1"
                        );
                        Ok(())
                    })
                }
            },
        )
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // 4 — P2 cannot follow: one host cannot meet a threshold of 2.
    // ------------------------------------------------------------------
    Scenario::with_ctx(format!("P2 cannot leave {hint} at 2 of 2"), ())
        .when(
            "P2 asks to leave and is refused before anything is written",
            {
                let party_id = party_id.clone();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        let base_serial = serial_on(&*f, f.p2.http, &party_id).await?;
                        let (status, body) = f
                            .post_expect_status(
                                f.p2.http,
                                "/v0/tenant/unhost",
                                &unhost_request(&party_id, &f.p2.participant_id, base_serial),
                            )
                            .await?;
                        anyhow::ensure!(
                            status == StatusCode::BAD_REQUEST
                                && body.contains("lower the threshold"),
                            "P2 leaving a 2-of-2 party answered {status}: {body}"
                        );
                        let after = serial_on(&*f, f.p2.http, &party_id).await?;
                        anyhow::ensure!(
                            after == base_serial,
                            "the refused unhost moved the serial from {base_serial} to {after}"
                        );
                        Ok(())
                    })
                }
            },
        )
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // 5 — a host that holds the party's contracts can still leave.
    // ------------------------------------------------------------------
    // orphan-marker is on P1 and P2 since step 3, so both can store the
    // party's contract. P3 stays out of this party.
    let held = ExternalKeyPair::generate();
    let held_seed = *held.seed();
    let held_hint = fresh_prefix("tenant-unhost-held");
    let held_party = held.party_id(&held_hint);
    info!("External party P2 will leave while holding its contracts: {held_party}");

    let mut scenario = Scenario::with_ctx(
        format!("P2 leaves {held_hint} while it holds the party's contracts"),
        (),
    )
    .when("the wallet onboards it on P1 and P2 at 1 of 2", {
        let hint = held_hint.clone();
        move |f, _| {
            let hint = hint.clone();
            Box::pin(async move {
                let hosts = [
                    (f.p1.http, f.p1.participant_id.as_str()),
                    (f.p2.http, f.p2.participant_id.as_str()),
                ];
                onboard_on_hosts(&*f, &hint, held_seed, &hosts, 1).await
            })
        }
    });
    for (name, port) in [("P1", f.p1.http), ("P2", f.p2.http)] {
        let party_id = held_party.clone();
        scenario = scenario.then(
            format!("{name} hosts the party"),
            Duration::from_secs(180),
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    probe_workflow_status(
                        &*f,
                        port,
                        &format!("/v0/tenant/{party_id}/status"),
                        "tenant-onboarding",
                    )
                    .await
                })
            },
        );
    }
    scenario
        .when("the party creates a contract that P2 stores too", {
            let party_id = held_party.clone();
            move |f, _| {
                let party_id = party_id.clone();
                let config = p1_config_held.clone();
                let wallet = ExternalKeyPair::from_seed(held_seed);
                Box::pin(async move {
                    grant_rights(&*f, P1_JSON_API, &party_id, "P1").await?;
                    let token = f.refresher.token().await?;
                    let created =
                        create_marker_as_converted(&config, &token, &party_id, &wallet, 0)
                            .await
                            .context("the party submitting through P1 before P2 leaves")?;
                    anyhow::ensure!(created == 1, "created {created} contract(s), not 1");
                    Ok(())
                })
            }
        })
        .when("P2 leaves through /v0/tenant/unhost", {
            let party_id = held_party.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    leave(&*f, f.p2.http, "P2", &party_id, &f.p2.participant_id).await
                })
            }
        })
        .then("P1 is the party's only host", Duration::from_secs(120), {
            let party_id = held_party.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    let state: Value = f
                        .probe_get_json(f.p1.http, &format!("/v0/tenant/{party_id}/state"))
                        .await?;
                    (state.get("host_count").and_then(Value::as_u64)? == 1).then_some(Ok(()))
                })
            }
        })
        .then("P2 no longer hosts the party", Duration::from_secs(60), {
            let party_id = held_party.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    let status = f
                        .get_status(f.p2.http, &format!("/v0/tenant/{party_id}/status"))
                        .await
                        .ok()?;
                    (status == StatusCode::NOT_FOUND).then_some(Ok(()))
                })
            }
        })
        .run(f)
        .await
}
