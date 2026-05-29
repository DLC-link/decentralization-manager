//! GX: Coordinator crash AFTER both peers have accepted and entered their
//! command-poll loop → resume on restart completes the workflow.
//!
//! This is the localnet analogue of devnet's #173 repro: the peer is
//! already mid-workflow when the coordinator dies. The 3-strike Noise-only
//! abort (removed by the #173 fix) would have killed this run; the probe
//! path now keeps the peer alive while the coordinator restarts.

use std::time::Duration;

use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation, processes};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("midresume");
    let instance = format!("{prefix}-creation");
    chaos::say("GX", &format!("starting onboarding with prefix {prefix}"));
    chaos::post_onboarding(f, &prefix).await?;

    // 1. Wait for both peers to RECEIVE invites AND ACCEPT them so they enter
    //    their command-poll loop before we kill the coordinator.
    let p2_inv =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let p3_inv =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    // Give the peer command-poll a moment to issue its first poll.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 2. Hard-kill P1 (coordinator).
    chaos::say("GX", "both peers accepted; hard-killing P1 mid-workflow");
    processes::restart_node(f, 1).await?;

    // 3. Without re-accepting (peers are still polling), the resumed
    //    coordinator should bring the workflow to completion.
    let p1_db = f.db_path(1);
    chaos::say("GX", "waiting for resumed run to reach completed");
    chaos::poll_until(Duration::from_secs(360), || async {
        Ok(matches!(
            db::workflow_run_status(&p1_db, &instance, "Coordinator")
                .await?
                .as_deref(),
            Some("completed")
        ))
    })
    .await?;

    chaos::say("GX", "mid-workflow coordinator resume verified");
    chaos::dismiss_p1(f, &instance).await;
    Ok(())
}
