use std::time::Duration;

use serde_json::Value;
use tracing::info;

use crate::common::{Fixture, scenario::Scenario};

// A peer's vetted packages become visible a few seconds after it uploads them;
// keep a generous margin for a loaded CI runner (#242).
const VETTING_DEADLINE: Duration = Duration::from_secs(120);

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: check_peer_dars");

    Scenario::new("compare-peers reports both peers reachable on every node")
        .given(
            "DARs distributed across all 3 nodes (from previous phase)",
            |_f, _| Box::pin(async move { Ok(()) }),
        )
        .then(
            "P1 sees P2 and P3 reachable with packages",
            VETTING_DEADLINE,
            |f, _| {
                let port = f.p1.http;
                let peer_a = f.p2.participant_id.clone();
                let peer_b = f.p3.participant_id.clone();
                Box::pin(async move { probe_compare_peers(&*f, port, &peer_a, &peer_b).await })
            },
        )
        .then(
            "P2 sees P1 and P3 reachable with packages",
            VETTING_DEADLINE,
            |f, _| {
                let port = f.p2.http;
                let peer_a = f.p1.participant_id.clone();
                let peer_b = f.p3.participant_id.clone();
                Box::pin(async move { probe_compare_peers(&*f, port, &peer_a, &peer_b).await })
            },
        )
        .then(
            "P3 sees P1 and P2 reachable with packages",
            VETTING_DEADLINE,
            |f, _| {
                let port = f.p3.http;
                let peer_a = f.p1.participant_id.clone();
                let peer_b = f.p2.participant_id.clone();
                Box::pin(async move { probe_compare_peers(&*f, port, &peer_a, &peer_b).await })
            },
        )
        .run(f)
        .await
}

// TODO(onledger-phases): add a scenario that asserts `/participants-status`
// reports each peer `Active` once its registry heartbeat lands (design D3).
// It replaces the reachability scenario this phase used to carry.

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
            // The topology read has not answered yet, or the peer has vetted
            // nothing so far; keep polling until the deadline.
            return None;
        }
        if let Some(ek) = error_kind
            && !ek.is_null()
        {
            return Some(Err(anyhow::anyhow!(
                "peer {id} reachable but error_kind set: {ek:?}"
            )));
        }
        // Distinguish "still propagating" from "read an empty list": a peer
        // that reports reachable with zero packages while this node has some
        // is a terminal read failure, not a slow one. Surface it so the
        // message is actionable instead of a deadline timeout.
        if packages.is_empty() && local_count > 0 {
            return Some(Err(anyhow::anyhow!(
                "peer {id} reachable but reported zero packages while local has {local_count} \
                 — the vetted-packages read returned nothing"
            )));
        }
        // (We deliberately do NOT assert packages.len() == local_count: in
        // any real Canton localnet, peers have different sets of
        // Canton-internal packages installed beyond the DARs we explicitly
        // distribute. SV nodes in particular bootstrap many more packages.
        // The empty-read check above (empty packages on a reachable peer
        // with non-empty local) is the meaningful invariant.)

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
        // Regression for #242: a peer whose topology read has not answered
        // yet must keep polling, not fail the scenario.
        let v = response(
            3,
            json!([
                peer("A", true, json!(null), 5),
                peer("B", false, json!("topology_read_failed"), 0),
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
                assert!(chain.contains("returned nothing"), "got: {chain}");
            }
            other => panic!("expected a terminal empty-read error, got {other:?}"),
        }
    }

    #[test]
    fn reachable_peer_with_error_kind_is_terminal_error() {
        let v = response(
            3,
            json!([
                peer("A", true, json!(null), 5),
                peer("B", true, json!("topology_read_failed"), 5),
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
