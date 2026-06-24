//! P2: Cross-node RetryWorkflow is best-effort for unreachable peers.
//!
//! Force a coordinator-side failure, restart only P2 (leave P3 dead), POST
//! /workflows/{instance}/retry. The retry endpoint must return 200 and the
//! run must remain visible in /workflows for operator inspection (P3 never
//! advances, so the run won't reach Completed). End by restarting P3 and
//! cancelling+dismissing so the suite isn't poisoned.
//!
//! Failure injection: coordinators no longer self-fail when peers go away
//! (wait-states are human-paced and may legitimately sit for hours), so dead
//! peers alone can't produce the Failed row this phase needs. Instead we kill
//! P1 and flip its persisted row to `failed` directly — simulating an
//! active-step error — then restart it; startup recovery leaves Failed rows
//! alone, so the run is retryable.

use std::time::Duration;

use serde_json::json;
use tokio::time::sleep;

use crate::common::{
    Fixture, chaos, db, invitations::post_accept_invitation, processes, types::WorkflowRunsResponse,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("retry-offline");
    let instance = format!("{prefix}-creation");
    chaos::say("P2", &format!("starting onboarding with prefix {prefix}"));
    chaos::post_onboarding(f, &prefix).await?;

    let p2_inv =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let p3_inv =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    // Brief settle so peer rows persist before we kill them.
    sleep(Duration::from_secs(3)).await;

    chaos::say("P2", "hard-killing both peers");
    processes::kill_node(f, 2).await?;
    processes::kill_node(f, 3).await?;

    // Kill P1 too and inject the coordinator failure into its DB while it's
    // down (the app is the sole writer of its sqlite file while running).
    chaos::say(
        "P2",
        "killing P1 and injecting coordinator failure into its DB",
    );
    processes::kill_node(f, 1).await?;
    let p1_db = f.db_path(1);
    db::inject_workflow_run_failure(
        &p1_db,
        &instance,
        "Coordinator",
        "chaos: injected active-step failure (P2 retry-with-offline-peer)",
    )
    .await?;

    // Restart P1 — recovery respawns inprogress rows only, so the run stays
    // Failed and retryable. Restart only P2; leave P3 offline.
    processes::spawn_only(f, 1).await?;
    processes::spawn_only(f, 2).await?;
    chaos::say(
        "P2",
        "P1 + P2 back up, P3 left dead; coordinator row Failed",
    );

    // POST retry — must return 200 even with P3 unreachable.
    chaos::say(
        "P2",
        "posting retry to /workflows/{instance}/retry (with P3 offline)",
    );
    let path = format!("/workflows/{instance}/retry");
    let (status, body) = f.post_expect_status(f.p1.http, &path, &json!({})).await?;
    anyhow::ensure!(
        status.as_u16() == 200,
        "retry returned {status}: {body} (expected 200 even with offline peer)"
    );
    chaos::say("P2", "retry POST accepted (HTTP 200) with P3 offline");

    // Give the coordinator time to react, then assert /workflows reflects
    // state honestly (P3 never advances; run will not reach Completed).
    sleep(Duration::from_secs(5)).await;
    let r: WorkflowRunsResponse = f.get_json(f.p1.http, "/workflows").await?;
    let n = r
        .runs
        .iter()
        .filter(|w| w.instance_name == instance)
        .count();
    anyhow::ensure!(n == 1, "/workflows hides the run after retry (count={n})");
    chaos::say(
        "P2",
        "/workflows surfaces the post-retry run for operator inspection",
    );

    // Restart P3 to unblock subsequent phases. Try to cancel + dismiss.
    chaos::say("P2", "restarting P3 to unblock subsequent phases");
    processes::spawn_only(f, 3).await?;
    let _ = f
        .post_expect_status(f.p1.http, "/onboarding/cancel", &json!({}))
        .await;
    sleep(Duration::from_secs(3)).await;
    chaos::dismiss_p1(f, &instance).await;
    Ok(())
}
