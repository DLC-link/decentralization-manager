use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use prometheus::{IntCounter, IntGaugeVec};
use tokio::sync::RwLock;

/// Shared map of `participant_id -> last time we observed authenticated traffic from that peer`.
pub type LastSeen = Arc<RwLock<HashMap<String, Instant>>>;

/// How long an entry stays "fresh". Within this window the ping loop skips probing.
/// Set to 3 ping-loop ticks (5s × 3) so a single missed observation doesn't trigger a probe.
pub const STALE_AFTER: Duration = Duration::from_secs(15);

/// Returns true if the peer should be probed by the heartbeat (no entry, or entry older than `STALE_AFTER`).
pub fn should_probe(map: &HashMap<String, Instant>, id: &str, now: Instant) -> bool {
    match map.get(id) {
        Some(t) => now.duration_since(*t) >= STALE_AFTER,
        None => true,
    }
}

/// Conditionally write `now` into the entry for `id`, but only if it would advance the timestamp.
/// Prevents a stale `now` (captured before a long-running probe) from overwriting a fresher value
/// written by a concurrent receive path.
pub fn bump(map: &mut HashMap<String, Instant>, id: String, now: Instant) {
    let entry = map.entry(id).or_insert(now);
    if now > *entry {
        *entry = now;
    }
}

static REACHABLE: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    prometheus::register_int_gauge_vec!(
        "decman_peer_reachable",
        "Whether this node can reach the peer: 1 reachable, 0 unreachable. The Noise ping loop sets it every 5s. Traffic heard from the peer in the last 15s also counts as reachable.",
        &["peer"]
    )
    .expect("metric name is a unique literal")
});

static PING_LOOP_TICKS: LazyLock<IntCounter> = LazyLock::new(|| {
    prometheus::register_int_counter!(
        "decman_peer_ping_loop_total",
        "Peer ping loop ticks that refreshed decman_peer_reachable, one every 5s. A flat rate means the gauge is stale."
    )
    .expect("metric name is a unique literal")
});

/// Registers both families at startup, so the tick counter exists before the
/// first tick.
pub(crate) fn register_metrics() {
    LazyLock::force(&REACHABLE);
    LazyLock::force(&PING_LOOP_TICKS);
}

/// A change in a peer's reachability that deserves one log line.
#[derive(Debug, PartialEq, Eq)]
pub enum Transition {
    Lost,
    Recovered,
}

/// Compares a peer's new verdict with its previous one. A peer first seen
/// unreachable counts as lost, so a node that starts while a peer is down
/// still reports it.
pub fn transition(previous: Option<bool>, reachable: bool) -> Option<Transition> {
    match (previous, reachable) {
        (Some(true) | None, false) => Some(Transition::Lost),
        (Some(false), true) => Some(Transition::Recovered),
        (Some(true) | None, true) | (Some(false), false) => None,
    }
}

/// Applies one tick's verdicts, keyed by participant id. An `Err` holds the
/// reason the peer is unreachable. A peer missing from `verdicts` is no longer
/// in the peers table, so its entry and its gauge series are dropped.
pub fn record_verdicts(
    status: &mut HashMap<String, bool>,
    verdicts: Vec<(String, Result<(), String>)>,
) {
    let listed: HashSet<&str> = verdicts.iter().map(|(peer, _)| peer.as_str()).collect();
    status.retain(|peer, _| {
        let still_listed = listed.contains(peer.as_str());
        if !still_listed {
            let _ = REACHABLE.remove_label_values(&[peer]);
        }
        still_listed
    });

    for (peer, verdict) in verdicts {
        let reachable = verdict.is_ok();
        match transition(status.get(&peer).copied(), reachable) {
            Some(Transition::Lost) => {
                let error = verdict.err().unwrap_or_default();
                tracing::warn!(peer = %peer, error = %error, "peer became unreachable");
            }
            Some(Transition::Recovered) => {
                tracing::info!(peer = %peer, "peer became reachable");
            }
            None => {}
        }
        REACHABLE
            .with_label_values(&[&peer])
            .set(i64::from(reachable));
        status.insert(peer, reachable);
    }
    PING_LOOP_TICKS.inc();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_probe_returns_true_for_missing_key() {
        let map = HashMap::new();
        assert!(should_probe(&map, "alice", Instant::now()));
    }

    #[test]
    fn should_probe_returns_false_for_fresh_entry() {
        let mut map = HashMap::new();
        let now = Instant::now();
        map.insert("alice".to_string(), now);
        assert!(!should_probe(&map, "alice", now + Duration::from_secs(5)));
    }

    #[test]
    fn should_probe_returns_true_for_stale_entry() {
        let mut map = HashMap::new();
        let now = Instant::now();
        map.insert("alice".to_string(), now);
        assert!(should_probe(&map, "alice", now + STALE_AFTER));
    }

    #[test]
    fn should_probe_returns_false_just_before_stale() {
        // Pins the comparison at `>= STALE_AFTER`: one nanosecond shy of the
        // threshold must NOT probe. Without this, flipping `>=` to `>` would
        // still pass the exact-boundary test above.
        let mut map = HashMap::new();
        let now = Instant::now();
        map.insert("alice".to_string(), now);
        assert!(!should_probe(
            &map,
            "alice",
            now + STALE_AFTER - Duration::from_nanos(1)
        ));
    }

    #[test]
    fn should_probe_returns_true_well_past_stale() {
        let mut map = HashMap::new();
        let now = Instant::now();
        map.insert("alice".to_string(), now);
        assert!(should_probe(&map, "alice", now + STALE_AFTER * 2));
    }

    #[test]
    fn bump_inserts_when_missing() {
        let mut map = HashMap::new();
        let now = Instant::now();
        bump(&mut map, "alice".to_string(), now);
        assert_eq!(map.get("alice"), Some(&now));
    }

    #[test]
    fn bump_advances_timestamp_when_newer() {
        let mut map = HashMap::new();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);
        map.insert("alice".to_string(), t0);
        bump(&mut map, "alice".to_string(), t1);
        assert_eq!(map.get("alice"), Some(&t1));
    }

    #[test]
    fn bump_does_not_move_timestamp_backwards() {
        let mut map = HashMap::new();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(5);
        map.insert("alice".to_string(), t1);
        bump(&mut map, "alice".to_string(), t0);
        assert_eq!(map.get("alice"), Some(&t1));
    }

    #[test]
    fn a_reachable_peer_that_fails_is_lost() {
        assert_eq!(transition(Some(true), false), Some(Transition::Lost));
    }

    #[test]
    fn a_peer_first_seen_unreachable_is_lost() {
        assert_eq!(transition(None, false), Some(Transition::Lost));
    }

    #[test]
    fn an_unreachable_peer_that_answers_has_recovered() {
        assert_eq!(transition(Some(false), true), Some(Transition::Recovered));
    }

    #[test]
    fn an_unchanged_verdict_is_not_a_transition() {
        assert_eq!(transition(Some(true), true), None);
        assert_eq!(transition(Some(false), false), None);
        assert_eq!(transition(None, true), None);
    }

    /// The exported gauge value for `peer`, or `None` when it has no series.
    fn reachable_series(peer: &str) -> Option<i64> {
        prometheus::gather()
            .iter()
            .find(|f| f.name() == "decman_peer_reachable")?
            .get_metric()
            .iter()
            .find(|m| {
                m.get_label()
                    .iter()
                    .any(|l| l.name() == "peer" && l.value() == peer)
            })
            .map(|m| m.get_gauge().get_value() as i64)
    }

    // Each test below uses its own peer ids, since these tests share a
    // process-wide metrics registry and run in parallel.

    #[test]
    fn the_tick_counter_is_exposed_before_the_first_tick() {
        register_metrics();
        assert!(
            prometheus::gather()
                .iter()
                .any(|f| f.name() == "decman_peer_ping_loop_total")
        );
    }

    #[test]
    fn a_tick_sets_the_gauge_from_each_verdict() {
        register_metrics();
        let (up, down) = ("up::1220gauge", "down::1220gauge");
        let mut status = HashMap::new();
        record_verdicts(
            &mut status,
            vec![
                (up.to_string(), Ok(())),
                (down.to_string(), Err("connection refused".to_string())),
            ],
        );

        assert_eq!(reachable_series(up), Some(1));
        assert_eq!(reachable_series(down), Some(0));
        assert_eq!(status.get(up), Some(&true));
        assert_eq!(status.get(down), Some(&false));
    }

    #[test]
    fn a_peer_that_recovers_reads_one_again() {
        let peer = "flapping::1220recover";
        let mut status = HashMap::new();
        record_verdicts(&mut status, vec![(peer.to_string(), Err("timeout".into()))]);
        assert_eq!(reachable_series(peer), Some(0));

        record_verdicts(&mut status, vec![(peer.to_string(), Ok(()))]);
        assert_eq!(reachable_series(peer), Some(1));
    }

    #[test]
    fn a_peer_removed_from_the_table_loses_its_series() {
        let (kept, deleted) = ("kept::1220prune", "deleted::1220prune");
        let mut status = HashMap::new();
        record_verdicts(
            &mut status,
            vec![(kept.to_string(), Ok(())), (deleted.to_string(), Ok(()))],
        );
        assert_eq!(reachable_series(deleted), Some(1));

        record_verdicts(&mut status, vec![(kept.to_string(), Ok(()))]);

        assert_eq!(
            reachable_series(deleted),
            None,
            "a deleted peer must not report its last value forever"
        );
        assert!(!status.contains_key(deleted));
        assert_eq!(reachable_series(kept), Some(1));
    }

    #[test]
    fn every_tick_moves_the_loop_counter() {
        let before = PING_LOOP_TICKS.get();
        record_verdicts(&mut HashMap::new(), Vec::new());
        assert!(PING_LOOP_TICKS.get() > before);
    }
}
