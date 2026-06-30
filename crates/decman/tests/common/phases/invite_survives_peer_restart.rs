//! G13: A delivered invitation survives a peer restart and is acceptable after.
//!
//! P1 starts an Onboarding inviting P2 and P3. Once the invitation is visible
//! on P2, hard-kill P2 BEFORE it accepts and respawn it. The pending
//! invitation is persisted (`pending_invitations` table) and reloaded at boot,
//! so after the restart P2 must still show the card, accept it, and the run
//! must drive through to Completed — the coordinator sat in its human-paced
//! `WaitingForPeers` the whole time.
//!
//! Distinct from G1 (coordinator restarts pre-accept) and G2 (peer restarts
//! AFTER accepting, mid-flight): this is the only phase exercising invitation
//! persistence across a peer process restart.

use std::time::Duration;

use anyhow::Context;
use serde_json::json;
use tracing::info;

use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation, processes};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: invite_survives_peer_restart");
    chaos::ensure_nodes_healthy(f).await?;

    let prefix = chaos::fresh_prefix("inv-restart");
    let instance = chaos::start_workflow_on(
        f,
        f.p1.http,
        "/onboarding",
        &json!({
            "party_id_prefix": prefix,
            "peer_ids": [f.p2.participant_id, f.p3.participant_id],
        }),
    )
    .await?;
    chaos::say("G13", &format!("P1 coordinating {instance}"));

    // Invitation delivered to P2 — then kill P2 before it accepts.
    let _ = chaos::wait_for_invite_for_instance(f, f.p2.http, &instance, Duration::from_secs(60))
        .await?;
    chaos::say("G13", "invite visible on P2; hard-killing P2 before accept");
    processes::restart_node(f, 2).await?;

    // The card must come back from the DB after the respawn.
    let p2_inv =
        chaos::wait_for_invite_for_instance(f, f.p2.http, &instance, Duration::from_secs(60))
            .await
            .context("invitation must survive the peer restart (persisted + reloaded)")?;
    chaos::say("G13", "invite survived P2 restart; accepting on both peers");

    let p3_inv =
        chaos::wait_for_invite_for_instance(f, f.p3.http, &instance, Duration::from_secs(60))
            .await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    // The run completes end to end on the post-restart accept.
    let p1_db = f.db_path(1);
    let instance_for_poll = instance.clone();
    chaos::poll_until(Duration::from_secs(360), || {
        let p1_db = p1_db.clone();
        let instance = instance_for_poll.clone();
        async move {
            let s = db::workflow_run_status(&p1_db, &instance, "Coordinator").await?;
            if s.as_deref() == Some("failed") || s.as_deref() == Some("cancelled") {
                anyhow::bail!("run ended {s:?} after post-restart accept");
            }
            Ok(s.as_deref() == Some("completed"))
        }
    })
    .await
    .context("waiting for the run to complete after the post-restart accept")?;
    chaos::say("G13", "run completed after post-restart accept");

    // Cleanup.
    chaos::dismiss_p1(f, &instance).await;
    for (port, db_path) in [(f.p2.http, f.db_path(2)), (f.p3.http, f.db_path(3))] {
        let leftovers = db::list_undismissed_terminal_runs(&db_path, &["Onboarding"], "Peer")
            .await
            .unwrap_or_default();
        for inst in leftovers {
            chaos::dismiss_on(f, port, &inst).await;
        }
    }

    chaos::say("G13", "invite-survives-peer-restart verified");
    Ok(())
}
