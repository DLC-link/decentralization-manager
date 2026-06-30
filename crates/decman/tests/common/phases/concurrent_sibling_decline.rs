//! G14: Declining one of two concurrent sibling invites fails only that run.
//!
//! Mirror of G12 from the peer side. P1 coordinates two Onboarding runs
//! (A = keep, B = decline), both inviting P2 and P3. P3 accepts both; P2
//! accepts A and DECLINES B. The decline carries the coordinator run's
//! `workflow_instance` and is routed by `Message::instance`, so it must:
//! - fail run B's coordinator row on P1 (`decline_matches_run` hits B, not A),
//! - cancel ONLY B's peer row on P3 via the instance-stamped teardown
//!   broadcast (`broadcast_cancel_to_others`).
//!
//! Sibling A — accepted by both peers — must still complete everywhere.

use std::time::Duration;

use anyhow::Context;
use serde_json::json;
use tracing::info;

use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: concurrent_sibling_decline");
    chaos::ensure_nodes_healthy(f).await?;

    let prefix_keep = chaos::fresh_prefix("dec-keep");
    let prefix_decline = chaos::fresh_prefix("dec-drop");
    let invitees = [f.p2.participant_id.clone(), f.p3.participant_id.clone()];

    let keep = chaos::start_workflow_on(
        f,
        f.p1.http,
        "/onboarding",
        &json!({ "party_id_prefix": prefix_keep, "peer_ids": invitees }),
    )
    .await?;
    let declined = chaos::start_workflow_on(
        f,
        f.p1.http,
        "/onboarding",
        &json!({ "party_id_prefix": prefix_decline, "peer_ids": invitees }),
    )
    .await?;
    chaos::say(
        "G14",
        &format!("P1 coordinating keep={keep} decline={declined}"),
    );

    // P3 accepts both runs; P2 accepts A and declines B.
    let inv_deadline = Duration::from_secs(60);
    let p3_keep = chaos::wait_for_invite_for_instance(f, f.p3.http, &keep, inv_deadline).await?;
    let p3_declined =
        chaos::wait_for_invite_for_instance(f, f.p3.http, &declined, inv_deadline).await?;
    post_accept_invitation(f, f.p3.http, &p3_keep).await?;
    post_accept_invitation(f, f.p3.http, &p3_declined).await?;

    // Wait for P3's B peer row so the teardown exercises the in-flight path.
    let p3_db = f.db_path(3);
    let declined_for_poll = declined.clone();
    chaos::poll_until(Duration::from_secs(30), || {
        let p3_db = p3_db.clone();
        let declined = declined_for_poll.clone();
        async move {
            let s = db::peer_run_status_by_coordinator_instance(&p3_db, &declined).await?;
            Ok(s.as_deref() == Some("inprogress"))
        }
    })
    .await
    .context("waiting for P3's peer row of the to-be-declined run")?;

    let p2_keep = chaos::wait_for_invite_for_instance(f, f.p2.http, &keep, inv_deadline).await?;
    let p2_declined =
        chaos::wait_for_invite_for_instance(f, f.p2.http, &declined, inv_deadline).await?;
    post_accept_invitation(f, f.p2.http, &p2_keep).await?;
    chaos::say("G14", "P2 declining run B");
    let (status, body) = f
        .post_expect_status(
            f.p2.http,
            "/invitations/decline",
            &json!({ "id": p2_declined }),
        )
        .await?;
    anyhow::ensure!(status.as_u16() == 200, "decline returned {status}: {body}");

    // B's coordinator row fails on P1 — and ONLY B's.
    let p1_db = f.db_path(1);
    let declined_for_poll = declined.clone();
    chaos::poll_until(Duration::from_secs(60), || {
        let p1_db = p1_db.clone();
        let declined = declined_for_poll.clone();
        async move {
            Ok(db::workflow_run_status(&p1_db, &declined, "Coordinator")
                .await?
                .as_deref()
                == Some("failed"))
        }
    })
    .await
    .context("waiting for declined run's coordinator row to fail")?;

    // P3's B peer row is cancelled by the instance-stamped teardown broadcast;
    // its A row keeps running.
    let declined_for_poll = declined.clone();
    chaos::poll_until(Duration::from_secs(60), || {
        let p3_db = p3_db.clone();
        let declined = declined_for_poll.clone();
        async move {
            let s = db::peer_run_status_by_coordinator_instance(&p3_db, &declined).await?;
            Ok(s.as_deref() == Some("cancelled"))
        }
    })
    .await
    .context("waiting for P3's declined-run peer row to cancel")?;
    let keep_p3 = db::peer_run_status_by_coordinator_instance(&p3_db, &keep).await?;
    anyhow::ensure!(
        keep_p3.as_deref() == Some("inprogress") || keep_p3.as_deref() == Some("completed"),
        "sibling A's peer row on P3 must survive B's decline, got {keep_p3:?}"
    );
    chaos::say("G14", "decline scoped correctly; waiting for sibling A");

    // Sibling A completes everywhere.
    let keep_for_poll = keep.clone();
    chaos::poll_until(Duration::from_secs(360), || {
        let p1_db = p1_db.clone();
        let keep = keep_for_poll.clone();
        async move {
            let s = db::workflow_run_status(&p1_db, &keep, "Coordinator").await?;
            if s.as_deref() == Some("failed") || s.as_deref() == Some("cancelled") {
                anyhow::bail!("sibling A ended {s:?} after B's decline");
            }
            Ok(s.as_deref() == Some("completed"))
        }
    })
    .await
    .context("waiting for sibling A to complete after B's decline")?;
    chaos::say("G14", "sibling A completed after B's decline");

    // Cleanup.
    chaos::dismiss_p1(f, &keep).await;
    chaos::dismiss_p1(f, &declined).await;
    for (port, db_path) in [(f.p2.http, f.db_path(2)), (f.p3.http, f.db_path(3))] {
        let leftovers = db::list_undismissed_terminal_runs(&db_path, &["Onboarding"], "Peer")
            .await
            .unwrap_or_default();
        for inst in leftovers {
            chaos::dismiss_on(f, port, &inst).await;
        }
    }

    chaos::say("G14", "concurrent sibling decline verified");
    Ok(())
}
