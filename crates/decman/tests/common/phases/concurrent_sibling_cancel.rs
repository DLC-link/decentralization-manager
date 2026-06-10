//! G12: Cancelling one of two concurrent sibling runs leaves the other intact.
//!
//! P1 coordinates two Onboarding runs at once (A = keep, B = cancel), both
//! inviting P2 and P3. P3 accepts both (so B is mid-flight there); P2 accepts
//! neither (so it holds two pending invites). Cancelling B via the
//! per-instance endpoint must then:
//! - flip B's coordinator row to cancelled on P1,
//! - drop ONLY B's pending invite on P2 — A's invite must survive (the
//!   instance-scoped CancelInvite, not legacy drop-everything-from-sender),
//! - flip ONLY B's peer row to cancelled on P3 — A's peer row keeps running.
//!
//! Sibling A must still drive through to Completed everywhere afterwards.
//!
//! This is the e2e for the scoped-cancel work (instance-stamped CancelInvite +
//! `coordinator_instance` on peer rows + `POST /workflows/{instance}/cancel`).

use std::time::Duration;

use anyhow::Context;
use serde_json::json;
use tracing::info;

use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: concurrent_sibling_cancel");
    chaos::ensure_nodes_healthy(f).await?;

    let prefix_keep = chaos::fresh_prefix("sib-keep");
    let prefix_cancel = chaos::fresh_prefix("sib-cancel");
    let invitees = [f.p2.participant_id.clone(), f.p3.participant_id.clone()];

    let keep = chaos::start_workflow_on(
        f,
        f.p1.http,
        "/onboarding",
        &json!({ "party_id_prefix": prefix_keep, "peer_ids": invitees }),
    )
    .await?;
    let cancel = chaos::start_workflow_on(
        f,
        f.p1.http,
        "/onboarding",
        &json!({ "party_id_prefix": prefix_cancel, "peer_ids": invitees }),
    )
    .await?;
    chaos::say(
        "G12",
        &format!("P1 coordinating keep={keep} cancel={cancel}"),
    );

    // Both invites must surface on both peers (4 cards). P3 accepts both so
    // run B is mid-flight there; P2 deliberately leaves both pending.
    let inv_deadline = Duration::from_secs(60);
    let p2_keep = chaos::wait_for_invite_for_instance(f, f.p2.http, &keep, inv_deadline).await?;
    let _p2_cancel =
        chaos::wait_for_invite_for_instance(f, f.p2.http, &cancel, inv_deadline).await?;
    let p3_keep = chaos::wait_for_invite_for_instance(f, f.p3.http, &keep, inv_deadline).await?;
    let p3_cancel =
        chaos::wait_for_invite_for_instance(f, f.p3.http, &cancel, inv_deadline).await?;
    post_accept_invitation(f, f.p3.http, &p3_keep).await?;
    post_accept_invitation(f, f.p3.http, &p3_cancel).await?;

    // Wait until P3's peer row for run B is persisted in-progress, so the
    // cancel exercises the in-flight-run path (not just the invite drop).
    let p3_db = f.db_path(3);
    let cancel_for_poll = cancel.clone();
    chaos::poll_until(Duration::from_secs(30), || {
        let p3_db = p3_db.clone();
        let cancel = cancel_for_poll.clone();
        async move {
            let s = db::peer_run_status_by_coordinator_instance(&p3_db, &cancel).await?;
            Ok(s.as_deref() == Some("inprogress"))
        }
    })
    .await
    .context("waiting for P3's peer row of the to-be-cancelled run")?;

    // Cancel run B by instance — the only unambiguous cancel with siblings.
    chaos::say("G12", "cancelling run B via /workflows/{instance}/cancel");
    let path = format!("/workflows/{cancel}/cancel");
    let (status, body) = f.post_expect_status(f.p1.http, &path, &json!({})).await?;
    anyhow::ensure!(
        status.as_u16() == 200,
        "per-instance cancel returned {status}: {body}"
    );

    // B's coordinator row flips to cancelled on P1.
    let p1_db = f.db_path(1);
    let cancel_for_poll = cancel.clone();
    chaos::poll_until(Duration::from_secs(30), || {
        let p1_db = p1_db.clone();
        let cancel = cancel_for_poll.clone();
        async move {
            Ok(db::workflow_run_status(&p1_db, &cancel, "Coordinator")
                .await?
                .as_deref()
                == Some("cancelled"))
        }
    })
    .await
    .context("waiting for B's coordinator row to flip cancelled")?;

    // P3: B's peer row flips cancelled via the instance-stamped CancelInvite —
    // while A's peer row keeps running.
    let cancel_for_poll = cancel.clone();
    chaos::poll_until(Duration::from_secs(60), || {
        let p3_db = p3_db.clone();
        let cancel = cancel_for_poll.clone();
        async move {
            let s = db::peer_run_status_by_coordinator_instance(&p3_db, &cancel).await?;
            Ok(s.as_deref() == Some("cancelled"))
        }
    })
    .await
    .context("waiting for P3's B peer row to flip cancelled")?;
    let keep_p3 = db::peer_run_status_by_coordinator_instance(&p3_db, &keep).await?;
    anyhow::ensure!(
        keep_p3.as_deref() == Some("inprogress") || keep_p3.as_deref() == Some("completed"),
        "sibling A's peer row on P3 must survive B's cancel, got {keep_p3:?}"
    );

    // P2: B's pending invite is dropped, A's invite SURVIVES.
    let keep_for_poll = keep.clone();
    let cancel_for_poll = cancel.clone();
    let f_imm = &*f;
    chaos::poll_until(Duration::from_secs(60), || {
        let keep = keep_for_poll.clone();
        let cancel = cancel_for_poll.clone();
        async move {
            let r: crate::common::types::PendingInvitationsResponse =
                f_imm.get_json(f_imm.p2.http, "/invitations").await?;
            let has_keep = r
                .invitations
                .iter()
                .any(|i| i.workflow_instance.as_deref() == Some(keep.as_str()));
            let has_cancel = r
                .invitations
                .iter()
                .any(|i| i.workflow_instance.as_deref() == Some(cancel.as_str()));
            if !has_keep {
                anyhow::bail!("sibling A's invite vanished from P2 — cancel was not scoped");
            }
            Ok(!has_cancel)
        }
    })
    .await
    .context("waiting for B's invite to drop off P2 (with A's surviving)")?;
    chaos::say(
        "G12",
        "scoped cancel verified: sibling invite + peer row survived",
    );

    // Sibling A must still complete end to end: P2 accepts its surviving
    // invite, and A drives through on all three nodes.
    post_accept_invitation(f, f.p2.http, &p2_keep).await?;
    let keep_for_poll = keep.clone();
    chaos::poll_until(Duration::from_secs(360), || {
        let p1_db = p1_db.clone();
        let keep = keep_for_poll.clone();
        async move {
            let s = db::workflow_run_status(&p1_db, &keep, "Coordinator").await?;
            if s.as_deref() == Some("failed") || s.as_deref() == Some("cancelled") {
                anyhow::bail!("sibling A ended {s:?} after B's cancel");
            }
            Ok(s.as_deref() == Some("completed"))
        }
    })
    .await
    .context("waiting for sibling A to complete after B's cancel")?;
    chaos::say("G12", "sibling A completed after B's cancel");

    // Cleanup: dismiss everything this phase created on every node.
    chaos::dismiss_p1(f, &keep).await;
    chaos::dismiss_p1(f, &cancel).await;
    for (port, db_path) in [(f.p2.http, f.db_path(2)), (f.p3.http, f.db_path(3))] {
        let leftovers = db::list_undismissed_terminal_runs(&db_path, &["Onboarding"], "Peer")
            .await
            .unwrap_or_default();
        for inst in leftovers {
            chaos::dismiss_on(f, port, &inst).await;
        }
    }

    chaos::say("G12", "concurrent sibling cancel verified");
    Ok(())
}
