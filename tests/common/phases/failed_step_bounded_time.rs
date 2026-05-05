//! P1: Failed step surfaces as Failed within bounded time.
//!
//! Hard-kill both attestors and DO NOT restart them — the coordinator must
//! give up within a bounded time and mark the run Failed. /workflows must
//! still surface the failed run for operator inspection. Restart attestors
//! at the end so subsequent phases run.

use std::time::Duration;

use crate::common::{
    Fixture, chaos, db, invitations::post_accept_invitation, processes, types::WorkflowRunsResponse,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("bounded-fail");
    let instance = format!("{prefix}-creation");
    chaos::say("P1", &format!("starting onboarding with prefix {prefix}"));
    chaos::post_onboarding(f, &prefix).await?;

    let p2_inv =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let p3_inv =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    chaos::say("P1", "hard-killing both attestors and leaving them dead");
    processes::kill_node(f, 2).await?;
    processes::kill_node(f, 3).await?;

    // Bounded wait for failure. Bash uses 120s; we mirror.
    let p1_db = f.db_path(1);
    chaos::poll_until(Duration::from_secs(120), || async {
        Ok(matches!(
            db::workflow_run_status(&p1_db, &instance, "Coordinator")
                .await?
                .as_deref(),
            Some("failed")
        ))
    })
    .await
    .map_err(|e| anyhow::anyhow!("coordinator did not mark Failed within 120s: {e}"))?;
    chaos::say("P1", "coordinator row marked Failed within bound");

    // Confirm /workflows surfaces the failed run for the operator.
    let r: WorkflowRunsResponse = f.get_json(f.p1.http, "/workflows").await?;
    let surfaced = r
        .runs
        .iter()
        .any(|w| w.instance_name == instance && w.status == "failed");
    anyhow::ensure!(
        surfaced,
        "/workflows did not surface the failed run for operator inspection"
    );

    // Restart attestors so subsequent phases aren't poisoned.
    chaos::say("P1", "restarting P2 and P3");
    processes::spawn_only(f, 2).await?;
    processes::spawn_only(f, 3).await?;

    chaos::say("P1", "failed-step bounded-time verified");
    chaos::dismiss_p1(f, &instance).await;
    Ok(())
}
