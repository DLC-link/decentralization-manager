//! Reproduction: onboarding a member whose participant is MISSING the DARs for
//! the party's contracts must not silently break.
//!
//! Mirrors the devnet incident where a node was onboarded without the packages
//! for the party's contracts. The add-party workflow has no DAR/vetting check
//! before it imports the ACS (`SyncAcs` uses `ContractImportMode::Validation`,
//! which re-validates every contract and needs the contract's package present
//! and vetted on the target). So if the target lacks the package, the import
//! fails.
//!
//! This phase, run last on the shared party, kicks P3, bulk-creates
//! `GovernanceRules` contracts for the party, unvets `governance-core` on P3
//! (the onboarding target), then re-adds P3 and asserts P3's AddParty peer run
//! FAILS at `SyncAcs` — i.e. the import cannot validate contracts whose package
//! P3 no longer has. It's the RED half of a reproduce-then-fix: the fix is a
//! DAR-vetting preflight that catches this before the disconnect/import window.
//!
//! Localnet-only: it drives P3's Canton admin API directly (`UnvetDar`), which
//! is only reachable from the test on localnet.

use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture, TestTarget, db,
    http::probe_workflow_status,
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
};

/// How many `GovernanceRules` contracts to pre-create for the party before the
/// (doomed) onboard. The missing-package import failure triggers regardless of
/// count — this is kept modest to bound IT time; the devnet incident carried
/// ~1000+. Bump if you want to exercise the large-ACS timing too.
const NUM_ORPHAN_CONTRACTS: usize = 200;

/// Unvet the DAR whose name contains `name_filter` on the participant reachable
/// at `admin_port`, returning its main package id. Used to make the onboarding
/// target unable to validate the party's contracts on import.
async fn unvet_dar_on_participant(admin_port: u16, name_filter: &str) -> anyhow::Result<String> {
    use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
        ListDarsRequest, UnvetDarRequest, package_service_client::PackageServiceClient,
    };

    let mut client = PackageServiceClient::connect(format!("http://127.0.0.1:{admin_port}"))
        .await
        .with_context(|| format!("connect PackageService on admin port {admin_port}"))?;

    let dars = client
        .list_dars(tonic::Request::new(ListDarsRequest {
            limit: 1000,
            filter_name: name_filter.to_string(),
        }))
        .await
        .context("ListDars")?
        .into_inner()
        .dars;

    let dar = dars
        .into_iter()
        .find(|d| d.name.contains(name_filter))
        .with_context(|| format!("no DAR matching '{name_filter}' on admin port {admin_port}"))?;
    let main_package_id = dar.main;

    client
        .unvet_dar(tonic::Request::new(UnvetDarRequest {
            main_package_id: main_package_id.clone(),
            synchronizer_id: None,
        }))
        .await
        .with_context(|| format!("UnvetDar {main_package_id}"))?;

    Ok(main_package_id)
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: add_party_missing_dar");

    if f.target != TestTarget::Localnet {
        info!("skipping add_party_missing_dar: localnet-only (needs P3's Canton admin API)");
        return Ok(());
    }
    if f.p3.canton_admin.is_none() {
        info!("skipping add_party_missing_dar: P3_CANTON_ADMIN not set");
        return Ok(());
    }

    Scenario::with_ctx(
        "re-add P3 while it is missing the contracts' DAR",
        InvitationIds::default(),
    )
    .given("party present with member parties", |f, _| {
        Box::pin(async move {
            f.party_id()?;
            f.party_prefix()?;
            f.p1_member_party
                .clone()
                .context("p1 member party not set")?;
            f.p2_member_party
                .clone()
                .context("p2 member party not set")?;
            Ok(())
        })
    })
    // 1. Kick P3 so the party is P1+P2 and P3 is a fresh onboarding target.
    .when("P1 kicks P3", |f, _| {
        Box::pin(async move {
            let party_id = f.party_id()?.to_string();
            let p3_uid = f.p3.participant_id.clone();
            let req = json!({
                "decentralized_party_id": party_id,
                "participant_id": p3_uid,
                "new_threshold": 2_i64,
            });
            let _: Value = f
                .post_json(f.p1.http, "/kick", &req)
                .await
                .context("POST /kick")?;
            Ok(())
        })
    })
    .then(
        "Kick invitation visible on P2",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p2.http, "Kick").await?;
                ctx.p2 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 accepts Kick invitation", |f, ctx| {
        Box::pin(async move {
            let id = ctx
                .p2
                .as_deref()
                .context("P2 kick invite id not set")?
                .to_string();
            post_accept_invitation(f, f.p2.http, &id)
                .await
                .context("accept Kick on P2")
        })
    })
    .then(
        "kick reaches completed",
        Duration::from_secs(240),
        |f, _| {
            Box::pin(
                async move { probe_workflow_status(&*f, f.p1.http, "/kick/status", "kick").await },
            )
        },
    )
    // 2. Bulk-create GovernanceRules for the party (now P1+P2).
    .when("create bulk GovernanceRules for the party", |f, _| {
        Box::pin(async move {
            let party_id = f.party_id()?.to_string();
            let p1m = f.p1_member_party.clone().context("p1m")?;
            let p2m = f.p2_member_party.clone().context("p2m")?;
            let p1_uid = f.p1.participant_id.clone();
            let p2_uid = f.p2.participant_id.clone();
            // On localnet the member party doubles as the operator party.
            let operator_party = p1m.clone();
            let contracts: Vec<Value> = (0..NUM_ORPHAN_CONTRACTS)
                .map(|i| {
                    json!({
                        "id": format!("orphan-rules-{i}"),
                        "name": "GovernanceRules",
                        "package_id": "#governance-core-v1",
                        "module_name": "Governance.Rules",
                        "entity_name": "GovernanceRules",
                        "fields": [
                            {"type": "decentralized_party"},
                            {"type": "party_set", "parties": [&p1m, &p2m]},
                            {"type": "int64", "value": 2},
                            {"type": "rel_time", "microseconds": 1800000000_i64},
                            {"type": "none"},
                        ],
                    })
                })
                .collect();
            let req = json!({
                "decentralized_party_id": party_id,
                "participant_ids": [p1_uid, p2_uid],
                "participant_parties": [&p1m, &p2m],
                "operator_party": operator_party,
                "contracts": contracts,
            });
            let _: Value = f
                .post_json(f.p1.http, "/contracts", &req)
                .await
                .context("POST /contracts")?;
            Ok(())
        })
    })
    .then(
        "Contracts invitation visible on P2",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p2.http, "Contracts").await?;
                ctx.p2 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 accepts Contracts invitation", |f, ctx| {
        Box::pin(async move {
            let id = ctx
                .p2
                .as_deref()
                .context("P2 contracts invite id not set")?
                .to_string();
            post_accept_invitation(f, f.p2.http, &id)
                .await
                .context("accept Contracts on P2")
        })
    })
    .then(
        "contracts workflow reaches completed",
        Duration::from_secs(600),
        |f, _| {
            Box::pin(async move {
                probe_workflow_status(&*f, f.p1.http, "/contracts/status", "contracts").await
            })
        },
    )
    // 3. Unvet governance-core on P3 (the onboarding target) so its import
    //    re-validation of the party's GovernanceRules contracts fails.
    .when("unvet governance-core on P3 (onboarding target)", |f, _| {
        Box::pin(async move {
            let admin = f.p3.canton_admin.context("P3 admin port not set")?;
            let pkg = unvet_dar_on_participant(admin, "governance-core")
                .await
                .context("unvet governance-core on P3")?;
            info!("unvetted governance-core ({pkg}) on P3 admin {admin}");
            Ok(())
        })
    })
    // 4. Re-add P3 — doomed: SyncAcs import cannot validate the party's
    //    contracts on a target missing their package.
    .when("P1 re-adds P3", |f, _| {
        Box::pin(async move {
            let party_id = f.party_id()?.to_string();
            let p3_uid = f.p3.participant_id.clone();
            let req = json!({
                "decentralized_party_id": party_id,
                "new_participant_id": p3_uid,
                "new_threshold": 2_i64,
                "previous_threshold": 2_i64,
            });
            let _: Value = f
                .post_json(f.p1.http, "/add-party", &req)
                .await
                .context("POST /add-party")?;
            Ok(())
        })
    })
    .then(
        "AddParty invitation visible on P2",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p2.http, "AddParty").await?;
                ctx.p2 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .then(
        "AddParty invitation visible on P3",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p3.http, "AddParty").await?;
                ctx.p3 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 accepts AddParty invitation", |f, ctx| {
        Box::pin(async move {
            let id = ctx
                .p2
                .as_deref()
                .context("P2 addparty invite id not set")?
                .to_string();
            post_accept_invitation(f, f.p2.http, &id)
                .await
                .context("accept AddParty on P2")
        })
    })
    .when("P3 accepts AddParty invitation", |f, ctx| {
        Box::pin(async move {
            let id = ctx
                .p3
                .as_deref()
                .context("P3 addparty invite id not set")?
                .to_string();
            post_accept_invitation(f, f.p3.http, &id)
                .await
                .context("accept AddParty on P3")
        })
    })
    // 5. THE REPRODUCTION: P3's AddParty peer run must FAIL at SyncAcs because
    //    its participant can't validate the party's contracts without the
    //    package. (Coordinator idles on a stuck peer, so we assert on the peer.)
    .then(
        "P3's AddParty peer run fails (import cannot validate missing-package contracts)",
        Duration::from_secs(420),
        |f, _| {
            Box::pin(async move {
                let p3_db = f.db_path(3);
                let instance = match db::current_inprogress_peer_instance(&p3_db, "AddParty").await
                {
                    Ok(Some(i)) => i,
                    _ => db::latest_peer_instance(&p3_db, "AddParty")
                        .await
                        .ok()
                        .flatten()?,
                };
                match db::workflow_run_status(&p3_db, &instance, "Peer").await {
                    Ok(Some(s)) if s.eq_ignore_ascii_case("failed") => Some(Ok(())),
                    _ => None,
                }
            })
        },
    )
    .run(f)
    .await
}
