//! G2: Peer crash mid-workflow → auto-resume re-fires trigger.
//!
//! Drive an Onboarding from P1 with both peers accepting up front, then
//! hard-kill P2 right after its peer row reaches `inprogress`, restart
//! it, and assert /onboarding/status reaches Completed and P2's row is the
//! same one (created_at unchanged, exactly one row).

use std::time::Duration;

use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation, processes};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("resume-peer");
    let instance = format!("{prefix}-creation");
    chaos::say("G2", &format!("starting onboarding with prefix {prefix}"));
    chaos::post_onboarding(f, &prefix).await?;

    // Both peers accept up front (so P2 is mid-flight when we kill it).
    let p2_inv =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let p3_inv =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    // Wait for the inprogress peer row, then capture its synthesized
    // instance_name so we can refer to it across the restart.
    let p2_db = f.db_path(2);
    let p2_peer_instance = wait_for_peer_instance(&p2_db).await?;

    // Capture created_at so we can assert the row is reused, not recreated.
    let created_before = db::workflow_run_created_at(&p2_db, &p2_peer_instance, "Peer")
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing created_at for P2 peer row"))?;

    chaos::say("G2", "hard-killing P2");
    processes::restart_node(f, 2).await?;

    chaos::say("G2", "waiting for resumed run to reach completed on P2");
    chaos::poll_until(Duration::from_secs(240), || async {
        Ok(matches!(
            db::workflow_run_status(&p2_db, &p2_peer_instance, "Peer")
                .await?
                .as_deref(),
            Some("completed")
        ))
    })
    .await?;

    let row_count = db::count_workflow_run_rows(&p2_db, &p2_peer_instance, "Peer").await?;
    anyhow::ensure!(row_count == 1, "expected 1 peer row, got {row_count}");
    let created_after = db::workflow_run_created_at(&p2_db, &p2_peer_instance, "Peer")
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing created_at for P2 peer row post-restart"))?;
    anyhow::ensure!(
        created_before == created_after,
        "created_at changed across restart ({created_before} → {created_after}) — \
         row was re-created, not reused"
    );

    chaos::say("G2", "peer resume verified (row reused, completed)");
    chaos::dismiss_p1(f, &instance).await;
    Ok(())
}

/// Spin until an `inprogress` Onboarding peer row appears at `db_path`,
/// returning its synthesized `instance_name`. Used by chaos phases that
/// can't use the generic `chaos::poll_until` helper because the value they
/// want to capture is not `Copy`.
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
