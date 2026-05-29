//! GX+1: Coordinator permanently dies → peers bail Failed after the 180 s
//! extended-tolerance budget expires. Verifies the bailout is bounded.
//!
//! Uses existing helpers: `processes::kill_node` (no restart) and
//! `processes::spawn_only` (restart later, for downstream phases). The peer
//! status check polls `db::latest_peer_instance` for the instance name,
//! then `db::workflow_run_status` to confirm it reached `failed`.

use std::time::{Duration, Instant};

use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation, processes};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("dead-coord");
    chaos::say(
        "GX+1",
        "starting onboarding then permanently killing coordinator",
    );
    chaos::post_onboarding(f, &prefix).await?;

    let p2_inv =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let p3_inv =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    // Give peer command-poll a moment to take its first poll.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Snapshot peer instance names BEFORE killing — needed to query their
    // terminal status later.
    let p2_db = f.db_path(2);
    let p3_db = f.db_path(3);
    let p2_inst = db::latest_peer_instance(&p2_db, "Onboarding")
        .await?
        .expect("p2 should have a peer instance");
    let p3_inst = db::latest_peer_instance(&p3_db, "Onboarding")
        .await?
        .expect("p3 should have a peer instance");

    // Kill coordinator (no restart).
    chaos::say("GX+1", "killing P1 permanently");
    processes::kill_node(f, 1).await?;

    // Both peers must bail Failed within (180 s budget + slack).
    let start = Instant::now();
    let p2_inst_ref = p2_inst.as_str();
    let p3_inst_ref = p3_inst.as_str();
    chaos::poll_until(Duration::from_secs(240), || async {
        let s2 = db::workflow_run_status(&p2_db, p2_inst_ref, "Peer").await?;
        let s3 = db::workflow_run_status(&p3_db, p3_inst_ref, "Peer").await?;
        Ok(s2.as_deref() == Some("failed") && s3.as_deref() == Some("failed"))
    })
    .await?;

    let elapsed = start.elapsed();
    anyhow::ensure!(
        elapsed >= Duration::from_secs(150),
        "peers bailed too early ({elapsed:?}); expected ≥150s (180s budget minus jitter)"
    );
    anyhow::ensure!(
        elapsed <= Duration::from_secs(230),
        "peers took too long to bail ({elapsed:?}); expected ≤230s"
    );

    chaos::say("GX+1", &format!("peers bailed Failed after {elapsed:?}"));

    // Restart P1 for downstream phases. `spawn_only` re-spawns without a
    // prior kill (we already killed above).
    processes::spawn_only(f, 1).await?;
    Ok(())
}
