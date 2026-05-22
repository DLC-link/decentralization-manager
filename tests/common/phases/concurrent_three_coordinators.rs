//! G11: Three concurrent coordinators with cross-acceptance.
//!
//! Every participant starts its own Onboarding workflow as coordinator,
//! inviting the other two as peers. Each peer accepts both incoming
//! invitations. All three coordinator runs must reach Completed.
//!
//! Exercises the architectural goal of the concurrent-workflows refactor:
//! a node mid-coordinator-workflow must still answer mesh-checks and
//! record incoming InviteX from other concurrent coordinators on the same
//! port that its workflow's `NoiseServer` is bound to.

use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation};

/// POST `/onboarding` on the given port with `peer_ids`, return the
/// freshly-minted `instance_name` from the response.
async fn post_onboarding_on(
    f: &Fixture,
    port: u16,
    prefix: &str,
    peer_ids: &[&str],
) -> anyhow::Result<String> {
    let req = json!({
        "party_id_prefix": prefix,
        "peer_ids": peer_ids,
    });
    let resp: Value = f
        .post_json(port, "/onboarding", &req)
        .await
        .with_context(|| format!("POST /onboarding on port {port} with prefix {prefix}"))?;
    resp.get("instance_name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("POST /onboarding response missing instance_name: {resp}"))
}

/// Look up the pending invitation id whose coordinator pubkey identifies it
/// as coming from the given expected prefix (we lift the prefix off the
/// invite payload that `record_invitation_for` persists). Falls back to the
/// first matching kind if the prefix is missing.
async fn wait_for_invite_with_prefix(
    f: &Fixture,
    port: u16,
    expected_prefix: &str,
    deadline: Duration,
) -> anyhow::Result<String> {
    let start = std::time::Instant::now();
    loop {
        let r: crate::common::types::PendingInvitationsResponse =
            f.get_json(port, "/invitations").await?;
        if let Some(inv) = r.invitations.into_iter().find(|i| {
            i.invitation_type == "Onboarding" && i.prefix.as_deref() == Some(expected_prefix)
        }) {
            return Ok(inv.id);
        }
        if start.elapsed() >= deadline {
            anyhow::bail!(
                "Onboarding invitation for prefix {expected_prefix} not visible on port {port} \
                 within {deadline:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: concurrent_three_coordinators");
    chaos::ensure_nodes_healthy(f).await?;

    let prefix_p1 = chaos::fresh_prefix("c3-p1");
    let prefix_p2 = chaos::fresh_prefix("c3-p2");
    let prefix_p3 = chaos::fresh_prefix("c3-p3");
    info!("Prefixes: P1={prefix_p1} P2={prefix_p2} P3={prefix_p3}");

    // Start all three coords back to back. They go through `post_json` →
    // server returns 202 immediately, then the workflow runs in a spawned
    // task; the only blocking work in our request thread is the
    // synchronous mesh-check, so issuing them sequentially still hits
    // "all three mid-workflow at once" within a handful of seconds.
    let p1_inst = post_onboarding_on(
        f,
        f.p1.http,
        &prefix_p1,
        &[&f.p2.participant_id, &f.p3.participant_id],
    )
    .await?;
    let p2_inst = post_onboarding_on(
        f,
        f.p2.http,
        &prefix_p2,
        &[&f.p1.participant_id, &f.p3.participant_id],
    )
    .await?;
    let p3_inst = post_onboarding_on(
        f,
        f.p3.http,
        &prefix_p3,
        &[&f.p1.participant_id, &f.p2.participant_id],
    )
    .await?;
    info!("Coord instances: P1={p1_inst} P2={p2_inst} P3={p3_inst}");

    // Each participant should see the other two coordinators' invitations.
    // Filter by prefix so we know which run each invitation belongs to.
    let inv_deadline = Duration::from_secs(60);
    let p1_from_p2 = wait_for_invite_with_prefix(f, f.p1.http, &prefix_p2, inv_deadline).await?;
    let p1_from_p3 = wait_for_invite_with_prefix(f, f.p1.http, &prefix_p3, inv_deadline).await?;
    let p2_from_p1 = wait_for_invite_with_prefix(f, f.p2.http, &prefix_p1, inv_deadline).await?;
    let p2_from_p3 = wait_for_invite_with_prefix(f, f.p2.http, &prefix_p3, inv_deadline).await?;
    let p3_from_p1 = wait_for_invite_with_prefix(f, f.p3.http, &prefix_p1, inv_deadline).await?;
    let p3_from_p2 = wait_for_invite_with_prefix(f, f.p3.http, &prefix_p2, inv_deadline).await?;
    info!("All six invitations visible across the three peers");

    // Accept all six sequentially. Each accept returns 202 fast (POSTs onto
    // the per-kind peer-job channel and returns), so the cross-acceptance
    // races still happen — between the per-instance spawned peer jobs on
    // each node, not between these HTTP calls.
    post_accept_invitation(f, f.p1.http, &p1_from_p2).await?;
    post_accept_invitation(f, f.p1.http, &p1_from_p3).await?;
    post_accept_invitation(f, f.p2.http, &p2_from_p1).await?;
    post_accept_invitation(f, f.p2.http, &p2_from_p3).await?;
    post_accept_invitation(f, f.p3.http, &p3_from_p1).await?;
    post_accept_invitation(f, f.p3.http, &p3_from_p2).await?;
    info!("All cross-invitations accepted; waiting for all coords to reach Completed");

    // Each coordinator must drive its run to Completed within the deadline.
    let p1_db = f.db_path(1);
    let p2_db = f.db_path(2);
    let p3_db = f.db_path(3);
    let p1_inst_poll = p1_inst.clone();
    let p2_inst_poll = p2_inst.clone();
    let p3_inst_poll = p3_inst.clone();
    chaos::poll_until(Duration::from_secs(600), || {
        let p1_db = p1_db.clone();
        let p2_db = p2_db.clone();
        let p3_db = p3_db.clone();
        let p1_inst = p1_inst_poll.clone();
        let p2_inst = p2_inst_poll.clone();
        let p3_inst = p3_inst_poll.clone();
        async move {
            let s1 = db::workflow_run_status(&p1_db, &p1_inst, "Coordinator").await?;
            let s2 = db::workflow_run_status(&p2_db, &p2_inst, "Coordinator").await?;
            let s3 = db::workflow_run_status(&p3_db, &p3_inst, "Coordinator").await?;
            // Surface any failed run immediately — no point waiting out the
            // full deadline if a workflow has already given up.
            for (label, st) in [("P1", &s1), ("P2", &s2), ("P3", &s3)] {
                if st.as_deref() == Some("failed") {
                    anyhow::bail!("{label} coordinator workflow failed");
                }
            }
            Ok([s1, s2, s3]
                .iter()
                .all(|s| s.as_deref() == Some("completed")))
        }
    })
    .await
    .context("waiting for all three coordinator workflows to complete")?;
    info!("All three coordinator workflows completed");

    // Best-effort cleanup so this phase doesn't leave six rows littering
    // the notifications feed of every subsequent phase.
    chaos::dismiss_p1(f, &p1_inst).await;
    chaos::dismiss_on(f, f.p2.http, &p2_inst).await;
    chaos::dismiss_on(f, f.p3.http, &p3_inst).await;
    // Peer rows too — each node has two peer rows from the other coords.
    let p1_peers = db::list_undismissed_terminal_runs(&p1_db, &["Onboarding"], "Peer")
        .await
        .unwrap_or_default();
    for inst in p1_peers {
        chaos::dismiss_p1(f, &inst).await;
    }
    let p2_peers = db::list_undismissed_terminal_runs(&p2_db, &["Onboarding"], "Peer")
        .await
        .unwrap_or_default();
    for inst in p2_peers {
        chaos::dismiss_on(f, f.p2.http, &inst).await;
    }
    let p3_peers = db::list_undismissed_terminal_runs(&p3_db, &["Onboarding"], "Peer")
        .await
        .unwrap_or_default();
    for inst in p3_peers {
        chaos::dismiss_on(f, f.p3.http, &inst).await;
    }

    info!("Phase concurrent_three_coordinators succeeded");
    // Silence "value never read" lint warnings for the helper bindings the
    // phase intentionally creates as documentation.
    let _ = (
        p1_from_p2, p1_from_p3, p2_from_p1, p2_from_p3, p3_from_p1, p3_from_p2,
    );
    Ok(())
}
