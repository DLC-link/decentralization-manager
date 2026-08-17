use std::time::Duration;

use serde_json::Value;
use tracing::info;

use crate::common::{Fixture, scenario::Scenario};

// Peers become reachable over the Noise mesh a few seconds after the
// configure_peers restart; keep a generous margin for a loaded CI runner (#242).
const MESH_CONVERGENCE_DEADLINE: Duration = Duration::from_secs(120);

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: check_peer_dars");

    Scenario::new("compare-peers reports both peers reachable on every node")
        .given(
            "DARs distributed across all 3 nodes (from previous phase)",
            |_f, _| Box::pin(async move { Ok(()) }),
        )
        .then(
            "P1 sees P2 and P3 reachable with packages",
            MESH_CONVERGENCE_DEADLINE,
            |f, _| {
                let port = f.p1.http;
                let peer_a = f.p2.participant_id.clone();
                let peer_b = f.p3.participant_id.clone();
                Box::pin(async move { probe_compare_peers(&*f, port, &peer_a, &peer_b).await })
            },
        )
        .then(
            "P2 sees P1 and P3 reachable with packages",
            MESH_CONVERGENCE_DEADLINE,
            |f, _| {
                let port = f.p2.http;
                let peer_a = f.p1.participant_id.clone();
                let peer_b = f.p3.participant_id.clone();
                Box::pin(async move { probe_compare_peers(&*f, port, &peer_a, &peer_b).await })
            },
        )
        .then(
            "P3 sees P1 and P2 reachable with packages",
            MESH_CONVERGENCE_DEADLINE,
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
    let v: Value = f.probe_get_json(port, "/packages/compare-peers").await?;
    classify_compare_peers(&v, expected_peer_a, expected_peer_b)
}

/// Poll outcome for a `/packages/compare-peers` response: `None` = keep
/// polling, `Some(Ok)` = both peers reachable with packages, `Some(Err)` =
/// terminal invariant violation.
fn classify_compare_peers(
    v: &Value,
    expected_peer_a: &str,
    expected_peer_b: &str,
) -> Option<anyhow::Result<()>> {
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
            // Not yet reachable — the Noise mesh is still converging; keep
            // polling until the deadline (mirrors probe_participants_status).
            return None;
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
        // message is actionable instead of a deadline timeout.
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
    let v: Value = f.probe_get_json(port, "/participants-status").await?;
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::classify_compare_peers;

    fn response(local_count: usize, peers: serde_json::Value) -> serde_json::Value {
        let local: Vec<serde_json::Value> = (0..local_count).map(|_| json!({})).collect();
        json!({ "local_packages": local, "peers": peers })
    }

    fn peer(
        id: &str,
        reachable: bool,
        error_kind: serde_json::Value,
        packages: usize,
    ) -> serde_json::Value {
        let packages: Vec<serde_json::Value> = (0..packages).map(|_| json!({})).collect();
        json!({
            "participant_id": id,
            "reachable": reachable,
            "error_kind": error_kind,
            "packages": packages,
        })
    }

    #[test]
    fn both_peers_reachable_with_packages_passes() {
        let v = response(
            3,
            json!([
                peer("A", true, json!(null), 5),
                peer("B", true, json!(null), 7)
            ]),
        );
        assert!(matches!(classify_compare_peers(&v, "A", "B"), Some(Ok(()))));
    }

    #[test]
    fn unreachable_peer_keeps_polling() {
        // Regression for #242: a transient unreachable peer while the Noise
        // mesh converges must keep polling, not fail the scenario.
        let v = response(
            3,
            json!([
                peer("A", true, json!(null), 5),
                peer("B", false, json!("transport"), 0),
            ]),
        );
        assert!(classify_compare_peers(&v, "A", "B").is_none());
    }

    #[test]
    fn reachable_peer_with_zero_packages_is_terminal_error() {
        let v = response(
            3,
            json!([
                peer("A", true, json!(null), 5),
                peer("B", true, json!(null), 0)
            ]),
        );
        match classify_compare_peers(&v, "A", "B") {
            Some(Err(e)) => {
                let chain = format!("{e:#}");
                assert!(chain.contains("decode failure"), "got: {chain}");
            }
            other => panic!("expected terminal decode-failure error, got {other:?}"),
        }
    }

    #[test]
    fn reachable_peer_with_error_kind_is_terminal_error() {
        let v = response(
            3,
            json!([
                peer("A", true, json!(null), 5),
                peer("B", true, json!("transport"), 5),
            ]),
        );
        assert!(matches!(classify_compare_peers(&v, "A", "B"), Some(Err(_))));
    }

    #[test]
    fn unexpected_peer_is_terminal_error() {
        let v = response(
            3,
            json!([
                peer("A", true, json!(null), 5),
                peer("X", true, json!(null), 5)
            ]),
        );
        assert!(matches!(classify_compare_peers(&v, "A", "B"), Some(Err(_))));
    }

    #[test]
    fn empty_local_packages_keeps_polling() {
        let v = response(0, json!([]));
        assert!(classify_compare_peers(&v, "A", "B").is_none());
    }

    #[test]
    fn wrong_peer_count_keeps_polling() {
        let v = response(3, json!([peer("A", true, json!(null), 5)]));
        assert!(classify_compare_peers(&v, "A", "B").is_none());
    }
}
