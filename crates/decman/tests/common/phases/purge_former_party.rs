//! Purge the contracts P3 still holds for the party it was kicked from, and
//! nothing else.
//!
//! Runs after `add_party_missing_dar`, which leaves P3 kicked from the main
//! party. P3 still holds the contracts it had for that party, and no longer
//! sees their archives. It still hosts the registrar dec party, which shares
//! contracts with the main party, and its own member party. The phase adds a
//! contract the main party shares with P3's member party, so P3 keeps seeing
//! that one after the kick.
//!
//! Asserts: P3 lists the main party as one it was removed from, and P1 does
//! not. A party P3 still hosts cannot be purged, and the main party cannot be
//! dismissed before its purge. The preview counts exactly the contracts the
//! test expects.
//!
//! A purge cut off by a DecMan restart leaves P3 off the synchronizer. The
//! restart marks that purge as failed, a new purge is refused while P3 is
//! off, and Reconnect brings P3 back.
//!
//! The purge then removes exactly the main party's contracts that no party P3
//! hosts is a stakeholder of. Every other contract P3 held is still there,
//! unless the ledger itself archived it. P3 is connected and healthy
//! afterwards. A second purge removes nothing, and the party can then be
//! dismissed.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    DisconnectSynchronizerRequest,
    synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
};
use chrono::Utc;
use common::{
    api::{
        AuditLogResponse, DecentralizedPartiesResponse, FormerPartiesResponse, FormerParty,
        PurgePreview, PurgeStatus,
    },
    types::AuditLogEntry,
};
use reqwest::StatusCode;
use serde_json::json;
use tracing::info;

use super::add_party_rehost_disconnect_first::{admin_config, connection_state, hosted_parties};
use crate::common::{
    Fixture, db,
    ledger_api::{P1_JSON_API, P3_JSON_API, SeedCoupon, reward_coupon_create_command},
    processes,
    scenario::Scenario,
};

/// Rounds from here up mark this phase's coupon apart from earlier phases'.
const ROUND_BASE: i64 = 200_000;

/// Below the reward automation's expiry margin, so its sweeps never assign
/// the coupon and it stays put for the whole phase.
const SEEDED_EXPIRY: chrono::Duration = chrono::Duration::seconds(60);

#[derive(Default)]
struct Ctx {
    /// The coupon the main party shares with P3's member party.
    shared_coupon: Option<String>,
    /// Every contract P3 held for the parties below before the purge, with
    /// its stakeholders.
    before: HashMap<String, Vec<String>>,
    /// How many of `before` the main party is a stakeholder of.
    party_contracts: usize,
    /// The main party's contracts no party P3 hosts is a stakeholder of.
    expected_purge: HashSet<String>,
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: purge_former_party");

    Scenario::with_ctx(
        "purge the contracts P3 holds for the party it was kicked from",
        Ctx::default(),
    )
    .given(
        "P3 is out of the main party and still hosts the registrar party",
        |f, _| {
            Box::pin(async move {
                let party = f.party_id()?.to_string();
                let registrar = registrar(f)?;
                f.p3_member_party()?;
                let p3 = admin_config(f, 3)?;
                let synchronizer_id = dec_party_manager::utils::get_synchronizer_id(&p3).await?;
                let hosted = hosted_parties(&p3, &synchronizer_id, &f.p3.participant_id).await?;
                anyhow::ensure!(
                    !hosted.contains(&party),
                    "P3 must be out of the main party; add_party_missing_dar kicks it"
                );
                anyhow::ensure!(
                    hosted.contains(&registrar),
                    "P3 must still host the registrar party, whose contracts the purge must keep"
                );
                Ok(())
            })
        },
    )
    .when(
        "a contract the main party shares with P3's member party is created on P3",
        |f, ctx| {
            Box::pin(async move {
                let party = f.party_id()?.to_string();
                let p3m = f.p3_member_party()?.to_string();
                let expires_at = (Utc::now() + SEEDED_EXPIRY)
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string();
                let cmd = reward_coupon_create_command(
                    &[SeedCoupon {
                        dso: p3m.clone(),
                        provider: party,
                        amount: "1.0".to_string(),
                        expires_at,
                        round: ROUND_BASE,
                    }],
                    &format!("purge-former-shared-{}", f.run_id),
                );
                f.submit_create(P3_JSON_API, &cmd)
                    .await
                    .context("create the shared coupon on P3")?;
                let coupon = f
                    .active_coupon_ids(P3_JSON_API, &p3m)
                    .await?
                    .into_iter()
                    .find(|(_, round)| *round == ROUND_BASE)
                    .map(|(cid, _)| cid)
                    .context("the shared coupon is not visible on P3")?;
                ctx.shared_coupon = Some(coupon);
                Ok(())
            })
        },
    )
    .then(
        "P3 has no workflow in flight",
        Duration::from_secs(300),
        |f, _| {
            Box::pin(async move {
                match db::in_progress_runs(&f.db_path(3)).await {
                    Ok(runs) if runs.is_empty() => Some(Ok(())),
                    Ok(runs) => {
                        f.probe_diag.record(
                            "P3 in-progress runs",
                            crate::common::probe::Class::Transient,
                            format!("{runs:?}"),
                        );
                        None
                    }
                    Err(e) => Some(Err(e)),
                }
            })
        },
    )
    .then(
        "P3 lists the main party as one it was removed from",
        Duration::from_secs(120),
        |f, _| {
            Box::pin(async move {
                let party = match f.party_id() {
                    Ok(v) => v.to_string(),
                    Err(e) => return Some(Err(e)),
                };
                let registrar = match registrar(f) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                // The refresh is what notices the removal.
                let _: DecentralizedPartiesResponse = f
                    .probe_get_json(f.p3.http, "/decentralized-parties?refresh=true")
                    .await?;
                let former: FormerPartiesResponse =
                    f.probe_get_json(f.p3.http, "/former-parties").await?;
                if former_entry(&former, &registrar).is_some() {
                    return Some(Err(anyhow::anyhow!(
                        "P3 lists the registrar party as former, but it still hosts it"
                    )));
                }
                let entry = former_entry(&former, &party)?;
                if entry.purge_status != PurgeStatus::None {
                    return Some(Err(anyhow::anyhow!(
                        "a fresh former party has purge status {:?}",
                        entry.purge_status
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "P1 lists neither party as former",
        Duration::from_secs(60),
        |f, _| {
            Box::pin(async move {
                let party = f.party_id().ok()?.to_string();
                let registrar = registrar(f).ok()?;
                let _: DecentralizedPartiesResponse = f
                    .probe_get_json(f.p1.http, "/decentralized-parties?refresh=true")
                    .await?;
                let former: FormerPartiesResponse =
                    f.probe_get_json(f.p1.http, "/former-parties").await?;
                if former_entry(&former, &party).is_some()
                    || former_entry(&former, &registrar).is_some()
                {
                    return Some(Err(anyhow::anyhow!(
                        "P1 hosts both parties, yet lists one as former: {:?}",
                        former
                            .parties
                            .iter()
                            .map(|p| p.party_id.to_string())
                            .collect::<Vec<_>>()
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .when("P3's contracts are recorded", |f, ctx| {
        Box::pin(async move {
            let party = f.party_id()?.to_string();
            let before = p3_contracts(f).await?;
            let p3 = admin_config(f, 3)?;
            let synchronizer_id = dec_party_manager::utils::get_synchronizer_id(&p3).await?;
            let mut hosted = hosted_parties(&p3, &synchronizer_id, &f.p3.participant_id).await?;
            hosted.insert(f.p3.participant_id.clone());
            hosted.remove(&party);

            let of_party: Vec<(&String, &Vec<String>)> = before
                .iter()
                .filter(|(_, stakeholders)| stakeholders.contains(&party))
                .collect();
            let expected_purge: HashSet<String> = of_party
                .iter()
                .filter(|(_, stakeholders)| !stakeholders.iter().any(|s| hosted.contains(s)))
                .map(|(cid, _)| (*cid).clone())
                .collect();
            let shared = ctx
                .shared_coupon
                .clone()
                .context("shared coupon not recorded")?;
            anyhow::ensure!(
                before.contains_key(&shared) && !expected_purge.contains(&shared),
                "the coupon shared with P3's member party must be held and kept"
            );
            anyhow::ensure!(
                !expected_purge.is_empty(),
                "P3 holds no leftovers of the main party, so the purge would prove nothing"
            );
            info!(
                "P3 holds {} contract(s) of the main party; {} are leftovers to purge",
                of_party.len(),
                expected_purge.len()
            );
            ctx.party_contracts = of_party.len();
            ctx.expected_purge = expected_purge;
            ctx.before = before;
            Ok(())
        })
    })
    .then(
        "a party P3 still hosts cannot be purged or previewed",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                let registrar = registrar(f).ok()?;
                let (status, body) = f
                    .post_expect_status(
                        f.p3.http,
                        "/former-parties/purge",
                        &json!({ "party_id": registrar }),
                    )
                    .await
                    .ok()?;
                if status != StatusCode::NOT_FOUND {
                    return Some(Err(anyhow::anyhow!(
                        "purging a hosted party returned {status}, not 404: {body}"
                    )));
                }
                let path = format!("/former-parties/purge-preview?party_id={registrar}");
                let (status, body) = f.get_expect_status(f.p3.http, &path).await.ok()?;
                if status != StatusCode::NOT_FOUND {
                    return Some(Err(anyhow::anyhow!(
                        "previewing a hosted party returned {status}, not 404: {body}"
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "the main party cannot be dismissed before its purge",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                let party = f.party_id().ok()?.to_string();
                let path = format!("/former-parties?party_id={party}");
                let (status, body) = f.delete_expect_status(f.p3.http, &path).await.ok()?;
                if status != StatusCode::CONFLICT {
                    return Some(Err(anyhow::anyhow!(
                        "dismissing before the purge returned {status}, not 409: {body}"
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "the preview counts the contracts the test expects",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let party = f.party_id().ok()?.to_string();
                let path = format!("/former-parties/purge-preview?party_id={party}");
                let preview: PurgePreview = f.probe_get_json(f.p3.http, &path).await?;
                let expected_keep = ctx.party_contracts - ctx.expected_purge.len();
                if preview.purge_count != ctx.expected_purge.len()
                    || preview.keep_count != expected_keep
                {
                    return Some(Err(anyhow::anyhow!(
                        "preview says purge {} keep {}, the test expects purge {} keep {}",
                        preview.purge_count,
                        preview.keep_count,
                        ctx.expected_purge.len(),
                        expected_keep
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .when(
        "a purge of the main party is cut off by a restart of P3's DecMan",
        |f, _| {
            Box::pin(async move {
                let party = f.party_id()?.to_string();
                processes::kill_node(f, 3).await?;
                // The state a purge leaves when DecMan dies during it: the
                // participant off the synchronizer, the purge still running.
                let p3 = admin_config(f, 3)?;
                SynchronizerConnectivityServiceClient::new(p3.admin_channel().await?)
                    .disconnect_synchronizer(tonic::Request::new(DisconnectSynchronizerRequest {
                        synchronizer_alias: p3.synchronizer().to_string(),
                    }))
                    .await
                    .context("disconnect P3")?;
                db::inject_running_purge(&f.db_path(3), &party).await?;
                processes::spawn_only(f, 3).await?;
                Ok(())
            })
        },
    )
    .then(
        "P3 lists the purge as cut off by the restart",
        Duration::from_secs(60),
        |f, _| {
            Box::pin(async move {
                let party = f.party_id().ok()?.to_string();
                let former: FormerPartiesResponse =
                    f.probe_get_json(f.p3.http, "/former-parties").await?;
                let entry = former_entry(&former, &party)?;
                if entry.purge_status != PurgeStatus::Failed {
                    return None;
                }
                let error = entry.purge_error.clone().unwrap_or_default();
                if !error.contains("restarted") {
                    return Some(Err(anyhow::anyhow!("unexpected purge error: {error}")));
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "a purge is refused while P3's participant is off the synchronizer",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                let party = f.party_id().ok()?.to_string();
                let (status, body) = f
                    .post_expect_status(
                        f.p3.http,
                        "/former-parties/purge",
                        &json!({ "party_id": party }),
                    )
                    .await
                    .ok()?;
                if status == StatusCode::CONFLICT && body.contains("not connected and healthy") {
                    Some(Ok(()))
                } else {
                    Some(Err(anyhow::anyhow!(
                        "a purge while P3 is off returned {status}: {body}"
                    )))
                }
            })
        },
    )
    .when(
        "the operator reconnects P3 from the main party's page",
        |f, _| {
            Box::pin(async move {
                let (status, body) = reconnect(f).await?;
                anyhow::ensure!(
                    status == StatusCode::OK,
                    "reconnect returned {status}: {body}"
                );
                Ok(())
            })
        },
    )
    .then(
        "P3's participant is connected and healthy after the reconnect",
        Duration::from_secs(90),
        |f, _| {
            Box::pin(async move {
                let p3 = admin_config(f, 3).ok()?;
                match connection_state(&p3).await {
                    Ok((true, false)) => Some(Ok(())),
                    _ => None,
                }
            })
        },
    )
    .then(
        "a second reconnect is refused while P3 is connected",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                let (status, body) = reconnect(f).await.ok()?;
                if status == StatusCode::CONFLICT && body.contains("already connected") {
                    Some(Ok(()))
                } else {
                    Some(Err(anyhow::anyhow!(
                        "a reconnect while P3 is connected returned {status}: {body}"
                    )))
                }
            })
        },
    )
    .when("the operator purges the main party on P3", |f, _| {
        Box::pin(async move { start_purge(f).await })
    })
    .then("the purge succeeds", Duration::from_secs(300), |f, ctx| {
        Box::pin(async move {
            let entry = purge_outcome(f).await?;
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => return Some(Err(e)),
            };
            let purged = ctx.expected_purge.len() as i64;
            if entry.purged_count != Some(purged) {
                return Some(Err(anyhow::anyhow!(
                    "the purge removed {:?} contract(s), the test expected {purged}",
                    entry.purged_count
                )));
            }
            Some(Ok(()))
        })
    })
    .then(
        "P3's participant is connected and healthy after the purge",
        Duration::from_secs(90),
        |f, _| {
            Box::pin(async move {
                let p3 = admin_config(f, 3).ok()?;
                match connection_state(&p3).await {
                    Ok((true, false)) => Some(Ok(())),
                    _ => None,
                }
            })
        },
    )
    .then(
        "only the main party's leftovers are gone from P3",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let party = f.party_id().ok()?.to_string();
                let after = p3_contracts(f).await.ok()?;
                let still_held: Vec<&String> = ctx
                    .expected_purge
                    .iter()
                    .filter(|cid| after.contains_key(*cid))
                    .collect();
                if !still_held.is_empty() {
                    return Some(Err(anyhow::anyhow!(
                        "{} leftover(s) are still active on P3: {still_held:?}",
                        still_held.len()
                    )));
                }
                let shared = ctx.shared_coupon.as_ref()?;
                if !after.contains_key(shared) {
                    return Some(Err(anyhow::anyhow!(
                        "the coupon shared with P3's member party was purged"
                    )));
                }

                // Anything else P3 no longer holds must be archived on the
                // ledger too. P1 hosts the main and the registrar party, so it
                // sees every one of those contracts that is still active.
                let gone: Vec<&String> = ctx
                    .before
                    .keys()
                    .filter(|cid| !after.contains_key(*cid) && !ctx.expected_purge.contains(*cid))
                    .collect();
                if !gone.is_empty() {
                    let on_p1 = p1_contracts(f).await.ok()?;
                    let wrongly_purged: Vec<&&String> =
                        gone.iter().filter(|cid| on_p1.contains(**cid)).collect();
                    if !wrongly_purged.is_empty() {
                        return Some(Err(anyhow::anyhow!(
                            "P3 lost {} contract(s) that are still active on the ledger: \
                             {wrongly_purged:?}",
                            wrongly_purged.len()
                        )));
                    }
                }
                let not_of_party: Vec<&String> = ctx
                    .expected_purge
                    .iter()
                    .filter(|cid| !ctx.before[*cid].contains(&party))
                    .collect();
                if !not_of_party.is_empty() {
                    return Some(Err(anyhow::anyhow!(
                        "purged contract(s) the main party is no stakeholder of: {not_of_party:?}"
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "the audit trail records who started the purge and what it removed",
        Duration::from_secs(30),
        |f, ctx| {
            Box::pin(async move {
                let party = f.party_id().ok()?.to_string();
                let path = format!("/governance/audit?party_id={party}");
                let trail: AuditLogResponse = f.probe_get_json(f.p3.http, &path).await?;
                let purges: Vec<&AuditLogEntry> = trail
                    .entries
                    .iter()
                    .filter(|e| e.action_summary == "purge_former_party")
                    .collect();
                // The cut-off purge left a start row too, so the trail holds
                // two starts and the one end of the purge that finished.
                let ends: Vec<&&AuditLogEntry> = purges
                    .iter()
                    .filter(|e| e.event_type == "purge_end")
                    .collect();
                let [end] = ends[..] else {
                    return Some(Err(anyhow::anyhow!(
                        "expected one purge_end row, found {}: {purges:?}",
                        ends.len()
                    )));
                };
                let starts = purges
                    .iter()
                    .filter(|e| e.event_type == "purge_start")
                    .count();
                if starts != 2 {
                    return Some(Err(anyhow::anyhow!(
                        "expected two purge_start rows, found {starts}: {purges:?}"
                    )));
                }
                if end.status != "success" {
                    return Some(Err(anyhow::anyhow!(
                        "the purge_end row is {}, not success: {end:?}",
                        end.status
                    )));
                }
                let expected = ctx.expected_purge.len() as u64;
                if end.details["purged_count"].as_u64() != Some(expected) {
                    return Some(Err(anyhow::anyhow!(
                        "purge_end records {} purged, expected {expected}",
                        end.details["purged_count"]
                    )));
                }
                if end.details["admin_sub"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty()
                {
                    return Some(Err(anyhow::anyhow!(
                        "purge_end records no admin user: {end:?}"
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .when("the operator purges the main party again", |f, _| {
        Box::pin(async move { start_purge(f).await })
    })
    .then(
        "the second purge removes nothing",
        Duration::from_secs(120),
        |f, _| {
            Box::pin(async move {
                let entry = match purge_outcome(f).await? {
                    Ok(entry) => entry,
                    Err(e) => return Some(Err(e)),
                };
                // Starting a purge clears the previous counts, so these are
                // this purge's own.
                match entry.purged_count {
                    Some(0) => Some(Ok(())),
                    other => Some(Err(anyhow::anyhow!(
                        "the second purge removed {other:?} contract(s), not 0"
                    ))),
                }
            })
        },
    )
    .when("the operator dismisses the main party", |f, _| {
        Box::pin(async move {
            let party = f.party_id()?.to_string();
            let path = format!("/former-parties?party_id={party}");
            let (status, body) = f.delete_expect_status(f.p3.http, &path).await?;
            anyhow::ensure!(
                status == StatusCode::OK,
                "dismiss returned {status}: {body}"
            );
            Ok(())
        })
    })
    .then(
        "P3 no longer lists the main party",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                let party = f.party_id().ok()?.to_string();
                let former: FormerPartiesResponse =
                    f.probe_get_json(f.p3.http, "/former-parties").await?;
                former_entry(&former, &party).is_none().then_some(Ok(()))
            })
        },
    )
    .run(f)
    .await
}

fn registrar(f: &Fixture) -> anyhow::Result<String> {
    f.registrar_party_id
        .clone()
        .context("registrar party not set — dual_governance_onboarding must run first")
}

fn former_entry<'a>(former: &'a FormerPartiesResponse, party: &str) -> Option<&'a FormerParty> {
    former
        .parties
        .iter()
        .find(|p| p.party_id.to_string() == party)
}

async fn start_purge(f: &Fixture) -> anyhow::Result<()> {
    let party = f.party_id()?.to_string();
    let (status, body) = f
        .post_expect_status(
            f.p3.http,
            "/former-parties/purge",
            &json!({ "party_id": party }),
        )
        .await?;
    anyhow::ensure!(
        status == StatusCode::ACCEPTED,
        "purge returned {status}: {body}"
    );
    Ok(())
}

async fn reconnect(f: &Fixture) -> anyhow::Result<(StatusCode, String)> {
    let party = f.party_id()?.to_string();
    f.post_expect_status(
        f.p3.http,
        "/former-parties/reconnect",
        &json!({ "party_id": party }),
    )
    .await
}

/// The main party's entry once its purge has finished. `None` while it runs;
/// an error when it failed.
async fn purge_outcome(f: &Fixture) -> Option<anyhow::Result<FormerParty>> {
    let party = f.party_id().ok()?.to_string();
    let former: FormerPartiesResponse = f.probe_get_json(f.p3.http, "/former-parties").await?;
    let entry = former_entry(&former, &party)?.clone();
    match entry.purge_status {
        PurgeStatus::Succeeded => Some(Ok(entry)),
        PurgeStatus::Failed => Some(Err(anyhow::anyhow!(
            "the purge failed: {}",
            entry.purge_error.unwrap_or_default()
        ))),
        PurgeStatus::Running | PurgeStatus::None => None,
    }
}

/// Every contract P3 holds for the main party, the registrar party and P3's
/// member party, with its stakeholders.
async fn p3_contracts(f: &Fixture) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let parties = [
        f.party_id()?.to_string(),
        registrar(f)?,
        f.p3_member_party()?.to_string(),
    ];
    let mut held = HashMap::new();
    for party in &parties {
        for contract in f.active_contracts(P3_JSON_API, party).await? {
            held.insert(contract.contract_id, contract.stakeholders);
        }
    }
    Ok(held)
}

/// Every contract id active on P1 for the main and the registrar party.
async fn p1_contracts(f: &Fixture) -> anyhow::Result<HashSet<String>> {
    let mut active = HashSet::new();
    for party in [f.party_id()?.to_string(), registrar(f)?] {
        for contract in f.active_contracts(P1_JSON_API, &party).await? {
            active.insert(contract.contract_id);
        }
    }
    Ok(active)
}
