//! Regression test for the UPSERT/COALESCE invariant introduced in Task 1
//! of plan #66 (defect B from design #64): a `/decentralized-parties` cache
//! refresh must NOT wipe a previously-resolved `owner_key`.
//!
//! Depends on:
//! - the `/decentralized-parties` response carrying a `refreshing: bool`
//!   flag that is `true` while the background `refresh_and_cache_parties`
//!   task is in progress;
//! - the server's 60s staleness threshold in
//!   `get_decentralized_parties` (`src/server/handlers/parties.rs`), which
//!   this phase crosses by backdating `dec_party.updated_at` rather than by
//!   waiting it out.
//!
//! If the server later makes refreshes synchronous or removes the
//! `refreshing` flag, this phase must be rewritten to trigger a refresh
//! through whatever the new mechanism is.

use std::time::Duration;

use anyhow::Context;
use common::{api::DecentralizedPartiesResponse, canton_id::CantonId};
use tokio::time::sleep;
use tracing::info;

use crate::common::{Fixture, db, scenario::Scenario};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: owner_key_resilience");

    Scenario::new("owner_key survives a cache refresh")
        .given("party + member parties present", |f, _| {
            Box::pin(async move {
                f.party_id()?;
                f.party_prefix()?;
                Ok(())
            })
        })
        .then(
            // Devnet-friendly cap. On localnet the resolution lands in ms;
            // on devnet the kubectl tunnel + cluster latency mean cache
            // staleness from earlier phases can take several seconds to
            // drain. 30s is the worst-case observed in practice.
            "P3 owner_key was resolved in an earlier phase",
            Duration::from_secs(30),
            |f, _| {
                Box::pin(async move {
                    let prefix = match f.party_prefix() {
                        Ok(v) => v.to_string(),
                        Err(e) => return Some(Err(e)),
                    };
                    let p3_uid: CantonId = match f.p3.participant_id.parse() {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let path = format!("/decentralized-parties?prefix={prefix}");
                    let r: DecentralizedPartiesResponse =
                        f.probe_get_json(f.p1.http, &path).await?;
                    let party = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id.prefix == prefix)?;
                    let pi = party
                        .participants
                        .into_iter()
                        .find(|p| p.participant_uid == p3_uid)?;
                    pi.owner_key.map(|_| Ok(()))
                })
            },
        )
        .when(
            "P1's cache is force-refreshed and the refresh completes",
            |f, _| {
                Box::pin(async move {
                    let prefix = f.party_prefix()?.to_string();
                    let path = format!("/decentralized-parties?prefix={prefix}");
                    let db_path = f.db_path(1);

                    // Quiesce before backdating. The preceding owner-key probe
                    // polled this same endpoint, and any GET there may have
                    // spawned a refresh. A task still in flight calls
                    // `store_parties_to_db` afterwards, stamping `updated_at`
                    // with the current time — clobbering the backdate below.
                    // The cache would then read fresh, no new refresh would
                    // fire, and this phase would pass without exercising the
                    // UPSERT/COALESCE invariant it exists to guard.
                    let mut quiesced = false;
                    for _ in 0..150 {
                        let r: DecentralizedPartiesResponse = f.get_json(f.p1.http, &path).await?;
                        if !r.refreshing {
                            quiesced = true;
                            break;
                        }
                        sleep(Duration::from_millis(200)).await;
                    }
                    anyhow::ensure!(
                        quiesced,
                        "a refresh was still in flight after 30s; backdating now would race it"
                    );

                    // Age the cache past the server's 60s staleness window so
                    // the next GET triggers `refresh_and_cache_parties`. The
                    // earlier phases' `/decentralized-parties` GETs keep
                    // resetting `updated_at`, so the cache is otherwise too
                    // fresh for the refresh to fire at all.
                    let aged =
                        db::backdate_dec_party_cache(&db_path, &prefix, Duration::from_secs(120))
                            .await?;
                    anyhow::ensure!(
                        aged > 0,
                        "no dec_party cache rows for prefix {prefix} — nothing to make stale, \
                         so the refresh under test would never fire"
                    );
                    let aged_at = db::dec_party_cache_updated_at(&db_path, &prefix)
                        .await?
                        .context("dec_party cache has no updated_at after backdating")?;

                    // Trigger the stale-cache path.
                    let _: DecentralizedPartiesResponse = f.get_json(f.p1.http, &path).await?;

                    // Wait for the refresh to have actually completed, proven by
                    // `updated_at` advancing past the aged value — a write only
                    // `store_parties_to_db` performs. The `refreshing` flag can't
                    // prove it: the spawned task can start and finish between two
                    // polls, so every observation reads false either way.
                    //
                    // 30s budget (150 × 200ms). Localnet refreshes in
                    // milliseconds; devnet's `resolve_owner_keys_from_peers`
                    // makes a Noise round trip per peer plus the Canton
                    // `list_my_owner_keys` admin-gRPC calls (post-#158 fix
                    // ≈3s/peer worst case on the kubectl tunnel). A run on
                    // 9fd91be exhausted the original 6s budget here.
                    for _ in 0..150 {
                        if db::dec_party_cache_updated_at(&db_path, &prefix)
                            .await?
                            .is_some_and(|t| t > aged_at)
                        {
                            return Ok(());
                        }
                        let _: DecentralizedPartiesResponse = f.get_json(f.p1.http, &path).await?;
                        sleep(Duration::from_millis(200)).await;
                    }
                    anyhow::bail!(
                        "cache updated_at never advanced past the aged value within 30s — \
                         refresh_and_cache_parties did not run, so this phase verified nothing"
                    )
                })
            },
        )
        .then(
            "P3's owner_key in P1's cache is still set",
            Duration::from_secs(30),
            |f, _| Box::pin(async move { Some(assert_owner_key_intact(f).await) }),
        )
        .run(f)
        .await
}

async fn assert_owner_key_intact(f: &mut Fixture) -> anyhow::Result<()> {
    let prefix = f.party_prefix()?.to_string();
    let p3_uid: CantonId = f.p3.participant_id.parse()?;
    let path = format!("/decentralized-parties?prefix={prefix}");
    let r: DecentralizedPartiesResponse = f.get_json(f.p1.http, &path).await?;
    let party = r
        .parties
        .into_iter()
        .find(|p| p.party_id.prefix == prefix)
        .context("party not found after refresh")?;
    let p3 = party
        .participants
        .into_iter()
        .find(|p| p.participant_uid == p3_uid)
        .context("P3 not in participants after refresh")?;
    p3.owner_key
        .context("P3 owner_key was wiped by refresh — UPSERT/COALESCE regression")?;
    Ok(())
}
