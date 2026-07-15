//! Reproduction + regression guard: onboarding a member whose participant is
//! MISSING a package the party's contracts use must be caught cleanly, not left
//! to fail mid-import (the devnet "onboarded a node without the DARs" incident).
//!
//! Setup (runs last, on the shared party): upload the tiny leaf `orphan-marker`
//! DAR to P1 and P2 ONLY, kick P3, bulk-create `OrphanMarker` contracts for the
//! party, then re-add P3 — whose participant never received `orphan-marker`. The
//! party's exported ACS now contains contracts P3 cannot validate.
//!
//! With the DAR-preflight fix, P3's AddParty peer must FAIL fast — before the
//! disconnect/import window — with an actionable missing-package error. This
//! phase asserts exactly that (peer run `failed`, error names the package),
//! which pins both the reproduction and the fix.

use std::{path::Path, time::Duration};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture, db,
    http::probe_workflow_status,
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
};

/// Contracts of the withheld package to seed into the party's ACS. The
/// missing-package failure triggers regardless of count; kept modest for IT time.
const NUM_ORPHAN_CONTRACTS: usize = 100;
const ORPHAN_DAR: &str = "orphan-marker-0.1.0.dar";

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: add_party_missing_dar");

    // The leaf DAR that P3 will NOT receive. Lives alongside the release DARs.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dar_path = Path::new(manifest_dir)
        .join("../../releases/v1")
        .join(ORPHAN_DAR);
    let dar_bytes = tokio::fs::read(&dar_path)
        .await
        .with_context(|| format!("reading {}", dar_path.display()))?;
    let upload_req = json!({
        "dar_files": [{ "filename": ORPHAN_DAR, "data": B64.encode(&dar_bytes) }],
    });

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
    // 1. Upload orphan-marker to P1 and P2 ONLY — never P3.
    .when("upload orphan-marker DAR to P1 and P2 (not P3)", {
        let upload_req = upload_req.clone();
        move |f, _| {
            let upload_req = upload_req.clone();
            Box::pin(async move {
                let _: Value = f
                    .post_json(f.p1.http, "/dars/upload", &upload_req)
                    .await
                    .context("upload orphan-marker to P1")?;
                let _: Value = f
                    .post_json(f.p2.http, "/dars/upload", &upload_req)
                    .await
                    .context("upload orphan-marker to P2")?;
                Ok(())
            })
        }
    })
    // 2. Kick P3 so it is a fresh onboarding target that lacks orphan-marker.
    .when("P1 kicks P3", |f, _| {
        Box::pin(async move {
            let party_id = f.party_id()?.to_string();
            let req = json!({
                "decentralized_party_id": party_id,
                "participant_id": f.p3.participant_id.clone(),
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
    // 3. Seed OrphanMarker contracts into the party's ACS (P1+P2 both have the DAR).
    .when("create OrphanMarker contracts for the party", |f, _| {
        Box::pin(async move {
            let party_id = f.party_id()?.to_string();
            let p1m = f.p1_member_party.clone().context("p1m")?;
            let p2m = f.p2_member_party.clone().context("p2m")?;
            let contracts: Vec<Value> = (0..NUM_ORPHAN_CONTRACTS)
                .map(|i| {
                    json!({
                        "id": format!("orphan-{i}"),
                        "name": "OrphanMarker",
                        "package_id": "#orphan-marker",
                        "module_name": "OrphanMarker",
                        "entity_name": "OrphanMarker",
                        "fields": [
                            {"type": "decentralized_party"},
                            {"type": "int64", "value": i as i64},
                        ],
                    })
                })
                .collect();
            let req = json!({
                "decentralized_party_id": party_id,
                "participant_ids": [f.p1.participant_id.clone(), f.p2.participant_id.clone()],
                "participant_parties": [&p1m, &p2m],
                "operator_party": p1m,
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
    // 4. Re-add P3 — its participant lacks orphan-marker.
    .when("P1 re-adds P3", |f, _| {
        Box::pin(async move {
            let party_id = f.party_id()?.to_string();
            let req = json!({
                "decentralized_party_id": party_id,
                "new_participant_id": f.p3.participant_id.clone(),
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
    // 5. The fix must catch it: P3's AddParty peer FAILS fast with a
    //    missing-package error (i.e. the preflight fired before any disconnect).
    .then(
        "P3's AddParty peer fails at the DAR preflight (missing orphan-marker)",
        Duration::from_secs(300),
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
                    Ok(Some(s)) if s.eq_ignore_ascii_case("failed") => {
                        let err = db::workflow_run_error(&p3_db, &instance, "Peer")
                            .await
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        if err.to_lowercase().contains("package") {
                            Some(Ok(()))
                        } else {
                            Some(Err(anyhow::anyhow!(
                                "P3 peer failed but not with a missing-package error: {err}"
                            )))
                        }
                    }
                    _ => None,
                }
            })
        },
    )
    .run(f)
    .await
}
