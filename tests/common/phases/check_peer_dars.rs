use std::time::Duration;

use serde_json::Value;
use tracing::info;

use crate::common::{Fixture, scenario::Scenario};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: check_peer_dars");

    Scenario::new("compare-peers reports both peers reachable on every node")
        .given(
            "DARs distributed across all 3 nodes (from previous phase)",
            |_f, _| Box::pin(async move { Ok(()) }),
        )
        .then(
            "P1 sees P2 and P3 reachable with packages",
            Duration::from_secs(60),
            |f, _| {
                let port = f.p1.http;
                let peer_a = f.p2.participant_id.clone();
                let peer_b = f.p3.participant_id.clone();
                Box::pin(async move { probe_compare_peers(&*f, port, &peer_a, &peer_b).await })
            },
        )
        .then(
            "P2 sees P1 and P3 reachable with packages",
            Duration::from_secs(60),
            |f, _| {
                let port = f.p2.http;
                let peer_a = f.p1.participant_id.clone();
                let peer_b = f.p3.participant_id.clone();
                Box::pin(async move { probe_compare_peers(&*f, port, &peer_a, &peer_b).await })
            },
        )
        .then(
            "P3 sees P1 and P2 reachable with packages",
            Duration::from_secs(60),
            |f, _| {
                let port = f.p3.http;
                let peer_a = f.p1.participant_id.clone();
                let peer_b = f.p2.participant_id.clone();
                Box::pin(async move { probe_compare_peers(&*f, port, &peer_a, &peer_b).await })
            },
        )
        .run(f)
        .await?;

    Scenario::new("participants-status reports each peer Connected on every node")
        .given("3-node mesh up (from previous phase)", |_f, _| {
            Box::pin(async move { Ok(()) })
        })
        .then(
            "P1 reports P2 and P3 Connected, self CurrentNode",
            Duration::from_secs(60),
            |f, _| {
                let port = f.p1.http;
                let self_id = f.p1.participant_id.clone();
                let peers = vec![f.p2.participant_id.clone(), f.p3.participant_id.clone()];
                Box::pin(
                    async move { probe_participants_status(&*f, port, &self_id, &peers).await },
                )
            },
        )
        .then(
            "P2 reports P1 and P3 Connected, self CurrentNode",
            Duration::from_secs(60),
            |f, _| {
                let port = f.p2.http;
                let self_id = f.p2.participant_id.clone();
                let peers = vec![f.p1.participant_id.clone(), f.p3.participant_id.clone()];
                Box::pin(
                    async move { probe_participants_status(&*f, port, &self_id, &peers).await },
                )
            },
        )
        .then(
            "P3 reports P1 and P2 Connected, self CurrentNode",
            Duration::from_secs(60),
            |f, _| {
                let port = f.p3.http;
                let self_id = f.p3.participant_id.clone();
                let peers = vec![f.p1.participant_id.clone(), f.p2.participant_id.clone()];
                Box::pin(
                    async move { probe_participants_status(&*f, port, &self_id, &peers).await },
                )
            },
        )
        .run(f)
        .await
}

async fn probe_compare_peers(
    f: &Fixture,
    port: u16,
    expected_peer_a: &str,
    expected_peer_b: &str,
) -> Option<anyhow::Result<()>> {
    let v: Value = f.get_json(port, "/packages/compare-peers").await.ok()?;

    let local_packages = v.get("local_packages")?.as_array()?;
    if local_packages.is_empty() {
        // Local DAR upload hasn't completed yet — keep polling.
        return None;
    }

    let peers = v.get("peers")?.as_array()?;
    if peers.len() != 2 {
        return None;
    }

    let local_count = local_packages.len();
    let mut seen_a = false;
    let mut seen_b = false;
    for peer in peers {
        let id = peer.get("participant_id")?.as_str()?;
        let reachable = peer.get("reachable")?.as_bool()?;
        let error_kind = peer.get("error_kind");
        let packages = peer.get("packages")?.as_array()?;

        if !reachable {
            return Some(Err(anyhow::anyhow!(
                "peer {id} reported unreachable (error_kind={error_kind:?})"
            )));
        }
        if let Some(ek) = error_kind
            && !ek.is_null()
        {
            return Some(Err(anyhow::anyhow!(
                "peer {id} reachable but error_kind set: {ek:?}"
            )));
        }
        // Distinguish "still propagating" from "responded with empty list":
        // if the peer reports reachable + zero packages while local has
        // some, that's the silent decode-failure path (Future work item 5
        // in the spec). Surface it as a terminal error so the failure
        // message is actionable instead of a 60s timeout.
        if packages.is_empty() && local_count > 0 {
            return Some(Err(anyhow::anyhow!(
                "peer {id} reachable but reported zero packages while local has {local_count} \
                 — likely decode failure (see spec Future work item 5)"
            )));
        }
        // (We deliberately do NOT assert packages.len() == local_count: in
        // any real Canton localnet, peers have different sets of
        // Canton-internal packages installed beyond the DARs we explicitly
        // distribute. SV nodes in particular bootstrap many more packages.
        // The decode-failure check above (empty packages on a reachable
        // peer with non-empty local) is the meaningful invariant.)

        if id == expected_peer_a {
            seen_a = true;
        } else if id == expected_peer_b {
            seen_b = true;
        } else {
            return Some(Err(anyhow::anyhow!(
                "unexpected peer in compare-peers response: {id}"
            )));
        }
    }

    if seen_a && seen_b { Some(Ok(())) } else { None }
}

async fn probe_participants_status(
    f: &Fixture,
    port: u16,
    self_id: &str,
    expected_peer_ids: &[String],
) -> Option<anyhow::Result<()>> {
    let v: Value = f
        .get_json(port, "/parties/participants-status")
        .await
        .ok()?;
    let statuses = v.get("statuses")?.as_array()?;

    if statuses.len() != expected_peer_ids.len() + 1 {
        return None;
    }

    for s in statuses {
        let id = s.get("id")?.as_str()?;
        let status = s.get("status")?.as_str()?;
        if id == self_id {
            if status != "CurrentNode" {
                return Some(Err(anyhow::anyhow!(
                    "self {self_id} reported status {status}, expected CurrentNode"
                )));
            }
        } else if expected_peer_ids.iter().any(|p| p == id) {
            if status != "Connected" {
                // Could be transient — keep polling until deadline.
                return None;
            }
        } else {
            return Some(Err(anyhow::anyhow!(
                "unexpected participant id in participants-status: {id}"
            )));
        }
    }

    Some(Ok(()))
}
