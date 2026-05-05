//! G1: Coordinator crash mid-workflow → auto-resume on restart.
//!
//! POST /onboarding on P1, wait for the coordinator row + invites delivered,
//! hard-kill P1, restart, accept on both attestors, and assert the run
//! reaches Completed via the persisted DB row (the in-memory
//! `<Kind>WorkflowState` is freshly constructed after restart and lags the
//! DB on slow runners). Invariant: exactly one coordinator row.

use std::time::Duration;

use crate::common::{
    Fixture, chaos, db, invitations::post_accept_invitation, processes,
    types::PendingInvitationsResponse,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("resume-coord");
    let instance = format!("{prefix}-creation");
    chaos::say("G1", &format!("starting onboarding with prefix {prefix}"));
    chaos::post_onboarding(f, &prefix).await?;

    // Wait for coordinator row inprogress AND for invites to actually reach
    // both attestors. The resume path doesn't re-send invites — killing P1
    // before the spawned task's 500ms ListenerPauseGuard + send_*_invites
    // runs leaves attestors with no pending invitation.
    let p1_db = f.db_path(1);
    let f_imm = &*f;
    chaos::poll_until(Duration::from_secs(60), || async {
        let n = db::count_workflow_runs_inprogress(&p1_db, "Onboarding", "Coordinator").await?;
        if n < 1 {
            return Ok(false);
        }
        let inv_p2: PendingInvitationsResponse =
            f_imm.get_json(f_imm.p2.http, "/invitations").await?;
        let inv_p3: PendingInvitationsResponse =
            f_imm.get_json(f_imm.p3.http, "/invitations").await?;
        let p2_has = inv_p2
            .invitations
            .iter()
            .any(|i| i.invitation_type == "Onboarding");
        let p3_has = inv_p3
            .invitations
            .iter()
            .any(|i| i.invitation_type == "Onboarding");
        Ok(p2_has && p3_has)
    })
    .await?;

    chaos::say("G1", "row + invites ready; hard-killing P1");
    processes::restart_node(f, 1).await?;

    // Now accept on both attestors.
    let p2_inv =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let p3_inv =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    chaos::say("G1", "waiting for resumed run to reach completed");
    let p1_db = f.db_path(1);
    chaos::poll_until(Duration::from_secs(240), || async {
        Ok(matches!(
            db::workflow_run_status(&p1_db, &instance, "Coordinator")
                .await?
                .as_deref(),
            Some("completed")
        ))
    })
    .await?;

    let row_count = db::count_workflow_run_rows(&p1_db, &instance, "Coordinator").await?;
    anyhow::ensure!(
        row_count == 1,
        "expected exactly 1 coordinator row, got {row_count}"
    );

    chaos::say("G1", "coordinator resume verified (single row, completed)");
    chaos::dismiss_p1(f, &instance).await;
    Ok(())
}
