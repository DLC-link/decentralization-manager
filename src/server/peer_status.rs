// src/server/peer_status.rs
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

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
}
