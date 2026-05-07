//! G9: Restart while two concurrent workflow kinds are in flight resumes both.
//!
//! Start an Onboarding then immediately a DARs distribution so both are
//! InProgress simultaneously. Defer accept on both kinds. Hard-kill P1,
//! restart, accept on both kinds, and assert both reach Completed by polling
//! the persisted rows (post-restart in-memory state can lag).

use std::{path::Path, time::Duration};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};

use crate::common::{
    Fixture, chaos, db, invitations::post_accept_invitation, processes,
    types::PendingInvitationsResponse,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;

    // Defensive: an earlier chaos phase that crashed/respawned P1 mid-flight
    // can leave dars_state pinned to InProgress with no task driving it
    // forward yet (recover_in_progress_workflows re-hydrates from the DB row,
    // and there is a brief window after respawn where status=InProgress but
    // abort_handle=None — /dars/cancel returns 409 "still initializing" in
    // that window, and the staleness watchdog hasn't yet flipped the row to
    // Failed). Poll until the API reports any non-InProgress status, retrying
    // the cancel each iteration. Bails after the deadline so the test fails
    // here with a clear message rather than later with a confusing 409.
    #[derive(serde::Deserialize, Debug)]
    struct DarsStatus {
        #[serde(default)]
        status: Option<String>,
    }
    let cancel_deadline = Duration::from_secs(120);
    let cancel_started = std::time::Instant::now();
    loop {
        let in_progress = match f
            .get_json::<DarsStatus>(f.p1.http, "/dars/distribute/status")
            .await
        {
            Ok(s) => matches!(s.status.as_deref(), Some("inprogress" | "InProgress")),
            // Transient HTTP failure (e.g., respawn settle) — retry until deadline.
            Err(_) => true,
        };
        if !in_progress {
            break;
        }
        if cancel_started.elapsed() >= cancel_deadline {
            anyhow::bail!(
                "G9 pre-test cleanup: dars_state stuck at InProgress on P1 after {cancel_deadline:?}; \
                 stale recovery never settled and cancel never landed"
            );
        }
        chaos::say(
            "G9",
            "cancelling stale in-progress Dars (will retry until cleared)",
        );
        let _ = f
            .post_expect_status(f.p1.http, "/dars/cancel", &json!({}))
            .await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let prefix = chaos::fresh_prefix("concurrent-kinds");
    let onboarding_instance = format!("{prefix}-creation");
    chaos::say("G9", &format!("starting onboarding with prefix {prefix}"));
    chaos::post_onboarding(f, &prefix).await?;

    chaos::say("G9", "starting parallel DARs distribute");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dars_dir = Path::new(manifest_dir).join("releases/v0/rc3");
    let dar_path = dars_dir.join("governance-core-v0-rc3-0.1.0.dar");
    let dar_b64 = B64.encode(
        tokio::fs::read(&dar_path)
            .await
            .with_context(|| format!("reading {}", dar_path.display()))?,
    );
    let req = json!({
        "dar_files": [{"filename": "governance-core-v0-rc3-0.1.0.dar", "data": dar_b64}],
        "peer_ids": [&f.p2.participant_id, &f.p3.participant_id],
    });
    let _: Value = f.post_json(f.p1.http, "/dars/distribute", &req).await?;

    // Wait for two distinct InProgress coordinator rows + invites of both
    // kinds delivered to both attestors (resume path doesn't re-send invites).
    let p1_db = f.db_path(1);
    let p1_db_clone = p1_db.clone();
    let f_imm: &Fixture = &*f;
    chaos::poll_until(Duration::from_secs(60), || async {
        let onb =
            db::count_workflow_runs_inprogress(&p1_db_clone, "Onboarding", "Coordinator").await?;
        let dars = db::count_workflow_runs_inprogress(&p1_db_clone, "Dars", "Coordinator").await?;
        if onb < 1 || dars < 1 {
            return Ok(false);
        }
        let inv_p2: PendingInvitationsResponse =
            f_imm.get_json(f_imm.p2.http, "/invitations").await?;
        let inv_p3: PendingInvitationsResponse =
            f_imm.get_json(f_imm.p3.http, "/invitations").await?;
        let kinds_p2: std::collections::HashSet<_> = inv_p2
            .invitations
            .iter()
            .map(|i| &i.invitation_type)
            .collect();
        let kinds_p3: std::collections::HashSet<_> = inv_p3
            .invitations
            .iter()
            .map(|i| &i.invitation_type)
            .collect();
        Ok(kinds_p2.len() >= 2 && kinds_p3.len() >= 2)
    })
    .await?;

    chaos::say("G9", "hard-killing P1 with both workflows in flight");
    processes::restart_node(f, 1).await?;

    // Accept all four pending invitations.
    let p2_onb =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    let p3_onb =
        chaos::wait_for_invite(f, f.p3.http, "Onboarding", Duration::from_secs(60)).await?;
    let p2_dars = chaos::wait_for_invite(f, f.p2.http, "Dars", Duration::from_secs(60)).await?;
    let p3_dars = chaos::wait_for_invite(f, f.p3.http, "Dars", Duration::from_secs(60)).await?;
    post_accept_invitation(f, f.p2.http, &p2_onb).await?;
    post_accept_invitation(f, f.p3.http, &p3_onb).await?;
    post_accept_invitation(f, f.p2.http, &p2_dars).await?;
    post_accept_invitation(f, f.p3.http, &p3_dars).await?;

    // Both must reach Completed in the DB.
    chaos::say("G9", "waiting for both kinds to complete");
    chaos::poll_until(Duration::from_secs(300), || async {
        Ok(matches!(
            db::workflow_run_status(&p1_db, &onboarding_instance, "Coordinator")
                .await?
                .as_deref(),
            Some("completed")
        ) && db::count_completed_runs(&p1_db, "Dars", "Coordinator").await? >= 1)
    })
    .await?;

    chaos::say("G9", "concurrent-kinds resume verified");

    // Cleanup: dismiss the rows we created.
    chaos::dismiss_p1(f, &onboarding_instance).await;
    let dars_leftovers = db::list_undismissed_terminal_runs(&p1_db, &["Dars"], "Coordinator")
        .await
        .unwrap_or_default();
    for inst in dars_leftovers {
        chaos::dismiss_p1(f, &inst).await;
    }
    Ok(())
}
