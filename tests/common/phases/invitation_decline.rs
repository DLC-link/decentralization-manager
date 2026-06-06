//! Invitation decline: a peer declining an outstanding invitation flips the
//! coordinator's matching in-progress run to `Failed`.
//!
//! Drives a fresh onboarding from P1, waits for the invite to land on P2, has
//! P2 DECLINE it (a peer-initiated `DeclineInvitation` Noise message), and
//! asserts the coordinator's `workflow_runs` row on P1 reaches `failed`.
//!
//! We assert on the persisted DB row, NOT the decline endpoint's HTTP
//! response: the coordinator-notify step is best-effort (the handler returns
//! 200 even if the Noise round-trip to the coordinator fails), so a green HTTP
//! would be a false positive. The phase generates its own fresh prefix and
//! dismisses the failed row before yielding.

use std::time::Duration;

use crate::common::{Fixture, chaos, db, invitations::post_decline_invitation};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("decline");
    let instance = format!("{prefix}-creation");
    chaos::say(
        "DECLINE",
        &format!("starting onboarding with prefix {prefix}"),
    );
    chaos::post_onboarding(f, &prefix).await?;

    // Wait for the invite to land on P2, then decline it.
    let p2_inv =
        chaos::wait_for_invite(f, f.p2.http, "Onboarding", Duration::from_secs(60)).await?;
    chaos::say("DECLINE", "P2 declining the onboarding invite");
    post_decline_invitation(f, f.p2.http, &p2_inv).await?;

    // The coordinator's matching in-progress run must flip to failed.
    let p1_db = f.db_path(1);
    chaos::poll_until(Duration::from_secs(120), || async {
        Ok(matches!(
            db::workflow_run_status(&p1_db, &instance, "Coordinator")
                .await?
                .as_deref(),
            Some("failed")
        ))
    })
    .await?;

    chaos::say(
        "DECLINE",
        "coordinator run flipped to failed on peer decline (verified)",
    );
    chaos::dismiss_p1(f, &instance).await;
    Ok(())
}
