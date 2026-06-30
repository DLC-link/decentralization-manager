//! G15: Pending invitations from one coordinator are capped, oldest evicted.
//!
//! With busy-gating gone, invites are recorded unconditionally — the cap
//! (`MAX_PENDING_INVITES_PER_COORDINATOR = 16`) is what keeps a buggy or
//! hostile (though authenticated) peer from growing `pending_invitations`
//! without bound. P1 starts 17 onboarding runs back to back (none accepted);
//! each peer must end up holding exactly the NEWEST 16 cards from P1, with
//! the first run's invite evicted.
//!
//! Cleanup cancels all 17 runs per-instance — the scoped CancelInvite
//! broadcasts also clear the surviving cards off the peers — and dismisses
//! the cancelled rows on P1.

use std::time::Duration;

use anyhow::Context;
use serde_json::json;
use tracing::info;

use crate::common::{Fixture, chaos, db, types::PendingInvitationsResponse};

const RUNS: usize = 17;
const CAP: usize = 16;

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: invite_cap");
    chaos::ensure_nodes_healthy(f).await?;

    let base = chaos::fresh_prefix("cap");
    let invitees = [f.p2.participant_id.clone(), f.p3.participant_id.clone()];

    let mut instances = Vec::with_capacity(RUNS);
    for i in 0..RUNS {
        let inst = chaos::start_workflow_on(
            f,
            f.p1.http,
            "/onboarding",
            &json!({ "party_id_prefix": format!("{base}-{i}"), "peer_ids": invitees }),
        )
        .await?;
        instances.push(inst);
    }
    chaos::say("G15", &format!("P1 coordinating {RUNS} onboarding runs"));

    // The 17th invite must arrive AND the 1st must be evicted, leaving the
    // newest 16. Invites are delivered by the runs' spawned tasks, so poll.
    let first = instances[0].clone();
    let last = instances[RUNS - 1].clone();
    let batch: Vec<String> = instances.clone();
    let f_imm = &*f;
    chaos::poll_until(Duration::from_secs(90), || {
        let first = first.clone();
        let last = last.clone();
        let batch = batch.clone();
        async move {
            let r: PendingInvitationsResponse =
                f_imm.get_json(f_imm.p2.http, "/invitations").await?;
            let present: Vec<&String> = batch
                .iter()
                .filter(|inst| {
                    r.invitations
                        .iter()
                        .any(|i| i.workflow_instance.as_deref() == Some(inst.as_str()))
                })
                .collect();
            let has_last = present.iter().any(|i| **i == last);
            let has_first = present.iter().any(|i| **i == first);
            if !has_last {
                // Still delivering — keep waiting.
                return Ok(false);
            }
            if has_first {
                anyhow::bail!(
                    "oldest invite was NOT evicted: {first} still present alongside {last} \
                     ({} of {RUNS} cards visible; cap is {CAP})",
                    present.len()
                );
            }
            if present.len() != CAP {
                anyhow::bail!(
                    "expected exactly the newest {CAP} cards from this batch, found {}",
                    present.len()
                );
            }
            Ok(true)
        }
    })
    .await
    .context("waiting for the invite cap to evict the oldest card on P2")?;
    chaos::say("G15", "cap verified on P2: newest 16 kept, oldest evicted");

    // Cleanup: cancel every run (instance-scoped CancelInvite clears the
    // surviving cards on the peers), then dismiss the cancelled rows on P1.
    for inst in &instances {
        let path = format!("/workflows/{inst}/cancel");
        let _ = f.post_expect_status(f.p1.http, &path, &json!({})).await;
    }
    let p1_db = f.db_path(1);
    let leftovers = db::list_undismissed_terminal_runs(&p1_db, &["Onboarding"], "Coordinator")
        .await
        .unwrap_or_default();
    for inst in leftovers {
        chaos::dismiss_p1(f, &inst).await;
    }

    chaos::say("G15", "invite cap verified");
    Ok(())
}
