//! G3: Coordinator-initiated retry of a Failed run flips peer rows back.
//!
//! Force a coordinator-side failure by killing both peers after they
//! accept. Restart peers. POST /workflows/{instance}/retry on P1.
//! Assert all three workflow_runs rows reach Completed.

use std::time::Duration;

use serde_json::json;

use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation, processes};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("retry-coord");
    let instance = format!("{prefix}-creation");
    chaos::say("G3", &format!("starting onboarding with prefix {prefix}"));
    chaos::post_onboarding(f, &prefix).await?;

    let p2_inv =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let p3_inv =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    // Wait for both peer rows to be persisted as inprogress, capturing
    // their synthesized instance_names so later assertions can refer to them
    // after they flip to terminal states.
    let p2_db = f.db_path(2);
    let p3_db = f.db_path(3);
    let p2_inst = wait_for_peer_instance(&p2_db).await?;
    let p3_inst = wait_for_peer_instance(&p3_db).await?;

    chaos::say("G3", "hard-killing both peers");
    processes::kill_node(f, 2).await?;
    processes::kill_node(f, 3).await?;

    // Wait for coordinator to mark Failed. We use the self-healing variant
    // because P1's noise listener has been observed to die mid-test (a
    // backend bug exposed by chaos sequences); without periodic
    // ensure_nodes_healthy the coordinator can stall and never reach Failed.
    let p1_db = f.db_path(1);
    {
        let p1_db = p1_db.clone();
        let instance_clone = instance.clone();
        chaos::poll_until_healthy(
            f,
            Duration::from_secs(600),
            Duration::from_secs(60),
            move |_| {
                let p1_db = p1_db.clone();
                let inst = instance_clone.clone();
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
    chaos::say("G3", "coordinator row marked Failed");

    // Restart peers so they're reachable for retry.
    processes::spawn_only(f, 2).await?;
    processes::spawn_only(f, 3).await?;

    // POST retry on P1.
    chaos::say("G3", "POSTing retry to /workflows/{instance}/retry");
    let path = format!("/workflows/{instance}/retry");
    let (status, body) = f.post_expect_status(f.p1.http, &path, &json!({})).await?;
    anyhow::ensure!(status.as_u16() == 200, "retry returned {status}: {body}");

    // Wait for P1 to flip to inprogress (or directly to completed).
    chaos::poll_until(Duration::from_secs(15), || async {
        let s = db::workflow_run_status(&p1_db, &instance, "Coordinator").await?;
        Ok(matches!(s.as_deref(), Some("inprogress" | "completed")))
    })
    .await?;

    // Now wait for completion on the persisted row.
    chaos::poll_until(Duration::from_secs(240), || async {
        Ok(matches!(
            db::workflow_run_status(&p1_db, &instance, "Coordinator")
                .await?
                .as_deref(),
            Some("completed")
        ))
    })
    .await?;

    // Wait until both peers flip back to completed (RetryWorkflow
    // re-broadcasts and the resumed runs go through to terminal completed).
    chaos::poll_until(Duration::from_secs(240), || async {
        let s2 = db::workflow_run_status(&p2_db, &p2_inst, "Peer").await?;
        let s3 = db::workflow_run_status(&p3_db, &p3_inst, "Peer").await?;
        Ok(s2.as_deref() == Some("completed") && s3.as_deref() == Some("completed"))
    })
    .await?;

    // Final assertions: all three rows Completed.
    let p1_final = db::workflow_run_status(&p1_db, &instance, "Coordinator").await?;
    let p2_final = db::workflow_run_status(&p2_db, &p2_inst, "Peer").await?;
    let p3_final = db::workflow_run_status(&p3_db, &p3_inst, "Peer").await?;
    anyhow::ensure!(
        p1_final.as_deref() == Some("completed")
            && p2_final.as_deref() == Some("completed")
            && p3_final.as_deref() == Some("completed"),
        "rows not all completed: P1={p1_final:?}, P2={p2_final:?}, P3={p3_final:?}"
    );

    chaos::say("G3", "retry-broadcast verified (all three rows completed)");
    chaos::dismiss_p1(f, &instance).await;
    Ok(())
}

async fn wait_for_peer_instance(db_path: &std::path::Path) -> anyhow::Result<String> {
    use std::time::Instant;
    let start = Instant::now();
    let deadline = Duration::from_secs(60);
    loop {
        if let Some(name) = db::current_inprogress_peer_instance(db_path, "Onboarding").await? {
            return Ok(name);
        }
        if start.elapsed() >= deadline {
            anyhow::bail!("inprogress peer row not visible at {db_path:?} within {deadline:?}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
