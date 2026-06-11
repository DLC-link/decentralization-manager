//! G11: Full-mesh concurrent workflows with cross-acceptance.
//!
//! Every participant simultaneously coordinates an Onboarding AND a DARs
//! distribution, inviting the other two as peers — six coordinator runs in
//! flight at once, and each node is concurrently: coordinator of 2 runs and
//! peer in 4 (two onboardings + two dars from the other coordinators).
//!
//! This is the architectural goal of the concurrent-workflows feature in one
//! phase, exercising end to end:
//! - instance-keyed routing over the always-on listener (`Message::instance`):
//!   each peer's command stream must reach the right coordinator run while
//!   five sibling runs are live;
//! - unconditional invite acceptance (no busy-gating) + per-run invitation
//!   cards (deduped by id, matched here via `workflow_instance`);
//! - the single peer-job queue running four concurrent `start_peer` loops per
//!   node;
//! - `coordinator_instance` persistence (migration 000014): peer-row
//!   completion is asserted by looking rows up via that column.

use std::{path::Path, time::Duration};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::json;
use tracing::info;

use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: concurrent_cross_workflows");
    chaos::ensure_nodes_healthy(f).await?;

    let nodes = [
        (1u8, f.p1.http, f.p1.participant_id.clone()),
        (2, f.p2.http, f.p2.participant_id.clone()),
        (3, f.p3.http, f.p3.participant_id.clone()),
    ];

    // Re-distributing a DAR every node already holds is an idempotent upload
    // on Canton, which is exactly what we want here — the phase exercises the
    // workflow machinery, not fresh package state.
    // DAR fixtures live at the workspace-root `releases/`; the crate is at
    // `crates/decman`, so resolve two levels up from CARGO_MANIFEST_DIR.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dar_path =
        Path::new(manifest_dir).join("../../releases/v0/rc3/governance-core-v0-rc3-0.1.0.dar");
    let dar_b64 = B64.encode(
        tokio::fs::read(&dar_path)
            .await
            .with_context(|| format!("reading {}", dar_path.display()))?,
    );

    // Start all six coordinator runs back to back. Each POST returns 202 once
    // the run is registered and spawned, so within a couple of seconds every
    // node is coordinating two runs while being invited into four.
    let mut onboarding_insts: Vec<(u8, u16, String)> = Vec::new();
    let mut dars_insts: Vec<(u8, u16, String)> = Vec::new();
    for (idx, port, _) in &nodes {
        let invitees: Vec<String> = nodes
            .iter()
            .filter(|(i, _, _)| i != idx)
            .map(|(_, _, pid)| pid.clone())
            .collect();

        let prefix = chaos::fresh_prefix(&format!("xwf-p{idx}"));
        let ob = chaos::start_workflow_on(
            f,
            *port,
            "/onboarding",
            &json!({ "party_id_prefix": prefix, "peer_ids": invitees }),
        )
        .await?;
        chaos::say("G11", &format!("P{idx} coordinating onboarding {ob}"));
        onboarding_insts.push((*idx, *port, ob));

        let dars = chaos::start_workflow_on(
            f,
            *port,
            "/dars/distribute",
            &json!({
                "dar_files": [{
                    "filename": "governance-core-v0-rc3-0.1.0.dar",
                    "data": dar_b64,
                }],
                "peer_ids": invitees,
            }),
        )
        .await?;
        chaos::say("G11", &format!("P{idx} coordinating dars {dars}"));
        dars_insts.push((*idx, *port, dars));
    }

    // Every node must see all four invitations from the other two coordinators
    // (their onboarding + their dars) — accept each as it appears.
    let inv_deadline = Duration::from_secs(60);
    let all_insts: Vec<(u8, u16, String)> = onboarding_insts
        .iter()
        .chain(dars_insts.iter())
        .cloned()
        .collect();
    for (coord_idx, _, inst) in &all_insts {
        for (peer_idx, peer_port, _) in &nodes {
            if peer_idx == coord_idx {
                continue;
            }
            let id = chaos::wait_for_invite_for_instance(f, *peer_port, inst, inv_deadline).await?;
            post_accept_invitation(f, *peer_port, &id).await?;
        }
    }
    chaos::say("G11", "all 12 cross-invitations visible and accepted");

    // All six coordinator runs must reach Completed; fail fast if any fails.
    let db_paths = [f.db_path(1), f.db_path(2), f.db_path(3)];
    let coord_targets: Vec<(usize, String)> = all_insts
        .iter()
        .map(|(idx, _, inst)| ((*idx as usize) - 1, inst.clone()))
        .collect();
    chaos::poll_until(Duration::from_secs(600), || {
        let db_paths = db_paths.clone();
        let targets = coord_targets.clone();
        async move {
            let mut all_done = true;
            for (node, inst) in &targets {
                let s = db::workflow_run_status(&db_paths[*node], inst, "Coordinator").await?;
                match s.as_deref() {
                    Some("completed") => {}
                    Some("failed") | Some("cancelled") => {
                        anyhow::bail!("coordinator run {inst} ended {s:?}")
                    }
                    _ => all_done = false,
                }
            }
            Ok(all_done)
        }
    })
    .await
    .context("waiting for all six coordinator runs to complete")?;
    chaos::say("G11", "all 6 coordinator runs completed");

    // Every peer-side row must complete too. Looking the rows up via
    // `coordinator_instance` (rather than scanning by kind) asserts the
    // migration-000014 linkage end to end.
    let peer_targets: Vec<(usize, String)> = all_insts
        .iter()
        .flat_map(|(coord_idx, _, inst)| {
            nodes
                .iter()
                .filter(move |(i, _, _)| i != coord_idx)
                .map(|(i, _, _)| ((*i as usize) - 1, inst.clone()))
        })
        .collect();
    chaos::poll_until(Duration::from_secs(120), || {
        let db_paths = db_paths.clone();
        let targets = peer_targets.clone();
        async move {
            for (node, coord_inst) in &targets {
                let s = db::peer_run_status_by_coordinator_instance(&db_paths[*node], coord_inst)
                    .await?;
                match s.as_deref() {
                    Some("completed") => {}
                    Some("failed") | Some("cancelled") => {
                        anyhow::bail!("peer run for {coord_inst} on node {node} ended {s:?}")
                    }
                    _ => return Ok(false),
                }
            }
            Ok(true)
        }
    })
    .await
    .context("waiting for all twelve peer runs to complete")?;
    chaos::say(
        "G11",
        "all 12 peer runs completed (coordinator_instance linkage verified)",
    );

    // Cleanup: dismiss every row this phase created so later phases (and the
    // operator feed) start clean.
    for (_, port, inst) in &all_insts {
        chaos::dismiss_on(f, *port, inst).await;
    }
    for (idx, port, _) in &nodes {
        let peers = db::list_undismissed_terminal_runs(
            &db_paths[(*idx as usize) - 1],
            &["Onboarding", "Dars"],
            "Peer",
        )
        .await
        .unwrap_or_default();
        for inst in peers {
            chaos::dismiss_on(f, *port, &inst).await;
        }
    }

    chaos::say("G11", "concurrent cross-workflows verified");
    Ok(())
}
