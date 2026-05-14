//! Regression test for the UPSERT/COALESCE invariant introduced in Task 1
//! of plan #66 (defect B from design #64): a `/decentralized-parties` cache
//! refresh must NOT wipe a previously-resolved `owner_key`.
//!
//! Depends on:
//! - the `/decentralized-parties` response carrying a `refreshing: bool`
//!   flag that is `true` while the background `refresh_and_cache_parties`
//!   task is in progress;
//! - the server's 60s staleness threshold (see
//!   `src/server/handlers/parties.rs:79`).
//!
//! If the server later makes refreshes synchronous or removes the
//! `refreshing` flag, this phase must be rewritten to trigger a refresh
//! through whatever the new mechanism is.

use std::time::Duration;

use anyhow::Context;
use tokio::time::sleep;
use tracing::info;

use crate::common::{Fixture, scenario::Scenario, types::DecentralizedPartiesResponse};

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
                    let prefix = f.party_prefix().ok()?.to_string();
                    let p3_uid = f.p3.participant_id.clone();
                    let path = format!("/decentralized-parties?prefix={prefix}");
                    let r: DecentralizedPartiesResponse =
                        f.get_json(f.p1.http, &path).await.ok()?;
                    let party = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id.starts_with(&prefix))?;
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
                    // Wait out the server's 60s staleness window so the next GET
                    // reliably triggers `refresh_and_cache_parties`. The previous
                    // phase's `/decentralized-parties` GETs reset `updated_at`,
                    // so without this wait the cache is too fresh and the
                    // refresh never fires in test timing (suite total ~3min).
                    sleep(Duration::from_secs(61)).await;

                    let prefix = f.party_prefix()?.to_string();
                    let path = format!("/decentralized-parties?prefix={prefix}");

                    // The 61s sleep above guarantees the next stale-cache GET
                    // triggers `refresh_and_cache_parties`. We don't insist on
                    // observing `refreshing == true` because the spawned task
                    // can complete between polls on a fast localnet, leaving
                    // every observation as `false` even though a refresh did
                    // run. Instead we poll until the response settles on
                    // `refreshing == false` and let the final assertion
                    // (owner_key intact) prove the invariant.
                    for _ in 0..30 {
                        let r: DecentralizedPartiesResponse = f.get_json(f.p1.http, &path).await?;
                        if !r.refreshing {
                            return Ok(());
                        }
                        sleep(Duration::from_millis(200)).await;
                    }
                    anyhow::bail!("refresh did not complete within 6s")
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
    let p3_uid = f.p3.participant_id.clone();
    let path = format!("/decentralized-parties?prefix={prefix}");
    let r: DecentralizedPartiesResponse = f.get_json(f.p1.http, &path).await?;
    let party = r
        .parties
        .into_iter()
        .find(|p| p.party_id.starts_with(&prefix))
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
