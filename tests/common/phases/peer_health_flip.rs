//! Peer-health flip: the always-on Noise listener + on-demand health probe
//! correctly report a peer as `Unreachable` when it is down and `Connected`
//! again after it restarts.
//!
//! Exercises the "reliable peer health" path end-to-end (issue #182):
//! `GET /participants-status` runs a live Noise `Health` round-trip to every
//! configured peer. We snapshot P1's baseline view, hard-kill P2, assert P1
//! flips it to `Unreachable`, restart P2, and assert P1 flips it back to
//! `Connected`.
//!
//! We target P2, matching every other chaos phase. The harness spawns and
//! tracks a PID for all three participant processes (`P1_PID`/`P2_PID`/
//! `P3_PID`); the third one's Canton identity is an `sv` (super-validator)
//! party, so the established convention is to kill/restart P2. We use
//! `kill_node` + `spawn_only` rather than `restart_node`: `kill_node` takes
//! (clears) the tracked PID, so `restart_node` — which needs one — would then
//! fail with "no tracked pid".
//!
//! No `workflow_runs` rows are created, so there is nothing to dismiss; the
//! phase restarts P2 before returning so later phases see a full mesh.

use std::time::Duration;

use serde::Deserialize;
use tracing::info;

use crate::common::{Fixture, chaos, processes};

#[derive(Debug, Deserialize)]
struct StatusResponse {
    statuses: Vec<PeerStatus>,
}

#[derive(Debug, Deserialize)]
struct PeerStatus {
    id: String,
    status: String,
}

/// P1's reported connection status for `peer_id`, or `None` if the peer is not
/// present in the response.
async fn p1_status_of(f: &Fixture, peer_id: &str) -> anyhow::Result<Option<String>> {
    let resp: StatusResponse = f.get_json(f.p1.http, "/participants-status").await?;
    Ok(resp
        .statuses
        .into_iter()
        .find(|s| s.id == peer_id)
        .map(|s| s.status))
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let p2_id = f.p2.participant_id.clone();

    // Snapshot the baseline once, logged for context if a later assertion
    // fails, and guard against an id-format mismatch up front.
    let snapshot: StatusResponse = f.get_json(f.p1.http, "/participants-status").await?;
    info!("[PH] baseline participant statuses (P1's view): {snapshot:?}");
    anyhow::ensure!(
        snapshot.statuses.iter().any(|s| s.id == p2_id),
        "P2 ({p2_id}) not present in /participants-status response — id-format mismatch?"
    );

    // Baseline: after `ensure_nodes_healthy`, P1 sees P2 Connected.
    chaos::say("PH", "asserting baseline P2 == Connected");
    chaos::poll_until(Duration::from_secs(60), || async {
        Ok(p1_status_of(f, &p2_id).await? == Some("Connected".to_string()))
    })
    .await?;

    // Kill P2 → P1's live probe must report it Unreachable (TCP connect fails).
    chaos::say("PH", "killing P2; expecting P1 to report Unreachable");
    processes::kill_node(f, 2).await?;
    chaos::poll_until(Duration::from_secs(60), || async {
        Ok(p1_status_of(f, &p2_id).await? == Some("Unreachable".to_string()))
    })
    .await?;

    // Respawn P2 → P1 must see it Connected again once the Noise mesh
    // reconverges. Use `spawn_only` (not `restart_node`): `kill_node` already
    // took the tracked PID, so `restart_node` would fail with "no tracked pid".
    chaos::say(
        "PH",
        "respawning P2; expecting P1 to report Connected again",
    );
    processes::spawn_only(f, 2).await?;
    chaos::poll_until(Duration::from_secs(120), || async {
        Ok(p1_status_of(f, &p2_id).await? == Some("Connected".to_string()))
    })
    .await?;

    chaos::say(
        "PH",
        "peer-health flip verified: Connected -> Unreachable -> Connected",
    );
    Ok(())
}
