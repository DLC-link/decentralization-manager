//! P2: Cross-node RetryWorkflow is best-effort for unreachable peers.
//!
//! Force a coordinator-side failure, restart only P2 (leave P3 dead), POST
//! /workflows/{instance}/retry. The retry endpoint must return 200 and the
//! run must remain visible in /workflows for operator inspection (P3 never
//! advances, so the run won't reach Completed). End by restarting P3 and
//! cancelling+dismissing so the suite isn't poisoned.

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

    // Brief settle so attestor rows persist before we kill them.
    sleep(Duration::from_secs(3)).await;

    chaos::say(
        "P2",
        "hard-killing both attestors to force coordinator failure",
    );
    processes::kill_node(f, 2).await?;
    processes::kill_node(f, 3).await?;

    let p1_db = f.db_path(1);
    {
        let p1_db = p1_db.clone();
        let inst = instance.clone();
        chaos::poll_until_healthy(
            f,
            Duration::from_secs(600),
            Duration::from_secs(60),
            move |_| {
                let p1_db = p1_db.clone();
                let inst = inst.clone();
                Box::pin(async move {
                    Ok(matches!(
                        db::workflow_run_status(&p1_db, &inst, "Coordinator")
                            .await?
                            .as_deref(),
                        Some("failed")
                    ))
                })
            },
        )
        .await?;
    }
    chaos::say("P2", "coordinator marked Failed");

    // Restart only P2; leave P3 offline.
    processes::spawn_only(f, 2).await?;

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
