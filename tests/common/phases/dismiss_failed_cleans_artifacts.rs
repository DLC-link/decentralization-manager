//! G4: Dismiss of a Failed run cascades artifact cleanup.
//!
//! Force a coordinator-side failure by killing both peers after they've
//! accepted the onboarding invite. Confirm artifacts exist for the failed
//! run; dismiss; confirm artifacts gone, the run row stays as dismissed=1,
//! and a fresh onboarding of the same kind starts (proving the unique
//! partial index is not blocked).

use std::time::Duration;

use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation, processes};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("dismiss-fail");
    let instance = format!("{prefix}-creation");
    chaos::say("G4", &format!("starting onboarding with prefix {prefix}"));
    chaos::post_onboarding(f, &prefix).await?;

    let p2_inv =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let p3_inv =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    // Wait for ≥1 artifact row before forcing failure.
    let p1_db = f.db_path(1);
    chaos::poll_until(Duration::from_secs(60), || async {
        Ok(db::count_artifacts(&p1_db, &instance).await? > 0)
    })
    .await?;

    chaos::say("G4", "hard-killing both peers to force coordinator failure");
    processes::kill_node(f, 2).await?;
    processes::kill_node(f, 3).await?;

    // Wait for coordinator to mark Failed (self-healing).
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

    // Confirm artifacts still exist for the failed run.
    let artifact_count_before = db::count_artifacts(&p1_db, &instance).await?;
    anyhow::ensure!(
        artifact_count_before >= 1,
        "failed run should keep artifacts; got {artifact_count_before}"
    );
    chaos::say(
        "G4",
        &format!("{artifact_count_before} artifact rows present pre-dismiss"),
    );

    // Dismiss the run.
    chaos::dismiss_p1(f, &instance).await;

    // Assert: artifacts gone, row stays with dismissed=1.
    let after = db::count_artifacts(&p1_db, &instance).await?;
    anyhow::ensure!(after == 0, "artifacts not cleaned (got {after})");
    let dismissed = db::workflow_run_dismissed(&p1_db, &instance, "Coordinator")
        .await?
        .ok_or_else(|| anyhow::anyhow!("row missing post-dismiss"))?;
    anyhow::ensure!(dismissed, "row not marked dismissed");
    chaos::say("G4", "artifacts cleaned, run row preserved as dismissed");

    // Restart peers so the fresh-start path can succeed.
    chaos::say("G4", "restarting P2 and P3");
    processes::spawn_only(f, 2).await?;
    processes::spawn_only(f, 3).await?;

    // Fresh onboarding of same kind should now succeed.
    let next_prefix = chaos::fresh_prefix("dismiss-fresh");
    let next_instance = format!("{next_prefix}-creation");
    chaos::say(
        "G4",
        &format!("starting fresh onboarding {next_prefix} to prove (kind, role) slot freed"),
    );
    chaos::post_onboarding(f, &next_prefix).await?;

    // Drive to completion.
    let next_p2 =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let next_p3 =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &next_p2).await?;
    post_accept_invitation(f, f.p3.http, &next_p3).await?;
    chaos::poll_until(Duration::from_secs(240), || async {
        Ok(matches!(
            db::workflow_run_status(&p1_db, &next_instance, "Coordinator")
                .await?
                .as_deref(),
            Some("completed")
        ))
    })
    .await?;

    chaos::say("G4", "dismiss + fresh-start path verified");
    chaos::dismiss_p1(f, &next_instance).await;
    Ok(())
}
