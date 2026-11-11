//! Discovery single-flight, its time budget and the per-prefix TTL record.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sqlx::SqlitePool;
use tokio::sync::{RwLock, Semaphore};

use super::{cache::store_parties_to_db, fetch::fetch_decentralized_parties, list::PartyReadOpts};
use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{NodeConfig, PartyCredentials},
    db::schema::SchemaRead,
    error::Result,
    party_discovery::known_party_filters,
    server::{
        AppState, former_parties,
        types::{DecentralizedPartiesResponse, DecentralizedParty},
    },
};

/// How many parties [`fetch_decentralized_parties`] reads at once.
///
/// The read used to await one future per hosted party together, so peak memory
/// scaled with the number of parties: each in-flight future holds an ACS read's
/// results and its gRPC buffers. A node hosting 213 parties ran 213 of those
/// concurrently and was OOMKilled at a 2Gi limit (#415). Bounding the
/// concurrency makes the peak track this number instead.
///
/// Small on purpose: the work is dominated by waiting on the participant, and
/// the node shares a 500m CPU limit with everything else it serves.
pub(super) const PARTY_READ_CONCURRENCY: usize = 8;

/// How long a completed party discovery answers requests before another one
/// runs, whether it found parties or not.
const PARTIES_CACHE_TTL_SECS: i64 = 60;

/// How long a request waits on another request's in-flight discovery before it
/// answers without data.
pub(super) const SINGLE_FLIGHT_WAIT: Duration = Duration::from_secs(3);

/// How many Canton discoveries may run at once, across every prefix.
///
/// Per-prefix single-flight does not bound this on its own: `prefix` comes from
/// the request, so a burst of distinct values would each start their own
/// discovery and put the concurrent load back on the participant.
pub(crate) const MAX_CONCURRENT_DISCOVERIES: usize = 4;

pub(super) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// Longest accepted `prefix`.
///
/// Wide enough for a whole party ID, identifier plus delimiter plus namespace,
/// so no legal filter is refused: a Canton identifier alone runs to 185
/// characters. It is a bound because the value keys an in-memory map.
pub(super) const MAX_PREFIX_LEN: usize = 255;

/// Cap on distinct prefixes tracked in `AppState::discovery_completed`.
const MAX_TRACKED_PREFIXES: usize = 1024;

/// Whether something recorded at `at` is still within the cache TTL.
///
/// `None` is never fresh. Neither is a timestamp ahead of `now`: the clock
/// moved backwards after it was written, and treating a negative age as fresh
/// would pin the answer until the clock caught up.
pub(super) fn is_within_ttl(at: Option<i64>, now: i64) -> bool {
    at.is_some_and(|at| (0..=PARTIES_CACHE_TTL_SECS).contains(&(now - at)))
}

/// Total wall clock one guarded discovery may take.
///
/// The permit is held for the whole of it, and the per-party contract and
/// metadata reads carry no deadline of their own, so without this a handful of
/// stalled Ledger API calls would hold every permit and turn each later
/// discovery into a 503. Generous on purpose: a cold fetch of 213 parties
/// measured 2m54s, almost all of it those reads.
const DISCOVERY_BUDGET: Duration = Duration::from_secs(300);

/// Drop expired entries, then the oldest, until `map` is under its cap.
///
/// Both prefix-keyed maps need this: `prefix` comes from the request, so an
/// unbounded one grows on demand. Eviction takes the oldest rather than
/// clearing, so a stream of unique prefixes cannot flush the entries real ones
/// depend on.
fn prune_and_cap<V: Copy>(map: &mut HashMap<String, V>, now: i64, at: impl Fn(&V) -> i64) {
    map.retain(|_, value| is_within_ttl(Some(at(value)), now));

    while map.len() >= MAX_TRACKED_PREFIXES {
        let Some(oldest) = map
            .iter()
            .min_by_key(|(_, value)| at(value))
            .map(|(prefix, _)| prefix.clone())
        else {
            break;
        };
        map.remove(&oldest);
    }
}

/// Whether a discovery completed for a prefix while a waiter was waiting.
///
/// Only a present, higher count counts. An entry that has gone missing means
/// eviction, not completion, and must never read as success: a waiter would
/// otherwise take the eviction as proof that fresh rows had been written.
pub(super) fn discovery_completed_since(entry: Option<u64>, current: Option<u64>) -> bool {
    match (entry, current) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(entry), Some(current)) => current > entry,
    }
}

/// Record that a discovery completed for `prefix`.
///
/// Only an empty result is marked. That is the one case the `dec_parties` cache
/// cannot represent, and marking a non-empty result would let a request
/// arriving before the cache write answer empty for the whole TTL.
///
/// A non-empty result clears any standing mark instead of leaving it: the
/// cached rows are the answer from that point, and if their write fails the
/// next request has to retry rather than read a stale mark as an empty result.
///
/// Expired entries go on the way in and the map is capped, because `prefix`
/// comes from the request.
async fn record_discovery(
    completed: &Arc<RwLock<HashMap<String, i64>>>,
    prefix: &str,
    parties: &[DecentralizedParty],
) {
    let now = now_secs();
    let mut completed = completed.write().await;

    if !parties.is_empty() {
        completed.remove(prefix);
        return;
    }

    // Re-stamping a prefix already tracked replaces its entry rather than
    // growing the map, so it must not cost another prefix its place.
    if !completed.contains_key(prefix) {
        prune_and_cap(&mut completed, now, |at| *at);
    }

    completed.insert(prefix.to_string(), now);
}

/// The shared bound every discovery runs under.
///
/// Cloned out of [`AppState`] so a background task can carry it without the
/// whole state. Calling [`fetch_decentralized_parties`] directly skips the
/// bound: two discoveries for one prefix then write the cache in an undefined
/// order, and the advertised concurrency cap stops meaning anything.
#[derive(Clone)]
pub(crate) struct DiscoveryGate {
    claims: Arc<RwLock<HashSet<String>>>,
    permits: Arc<Semaphore>,
    completed: Arc<RwLock<HashMap<String, i64>>>,
    generations: Arc<RwLock<HashMap<String, (u64, i64)>>>,
}

impl DiscoveryGate {
    /// The gate the HTTP paths and every background task share.
    pub(crate) fn of(state: &AppState) -> Self {
        Self {
            claims: state.refreshing_prefixes.clone(),
            permits: state.discovery_permits.clone(),
            completed: state.discovery_completed.clone(),
            generations: state.discovery_generations.clone(),
        }
    }
}

/// What a guarded discovery did.
pub(crate) enum Discovery {
    /// It ran, and the cache now holds this.
    Done(DecentralizedPartiesResponse),
    /// Another discovery for this prefix was already running.
    InFlight,
    /// Another discovery for this prefix finished while this one queued for
    /// capacity, so the cache already holds a fresh answer.
    Superseded,
    /// Every discovery permit was taken.
    AtCapacity,
    /// Canton refused or timed out.
    Failed(anyhow::Error),
}

/// Run one discovery for `prefix`, under the per-prefix claim and the global
/// bound, and leave the result recorded and cached.
///
/// Every entry point goes through here: the request path, the stale-cache
/// refresh, the startup sync and the post-workflow refreshes.
///
/// Three steps, in this order for a reason. Reading the claim first keeps a
/// duplicate from ever holding a permit while it waits. Taking the permit
/// before claiming keeps the claim set bounded by the permit count, since
/// `prefix` can come from a request and a burst of distinct values would
/// otherwise each add an entry and sit there. Recording and caching before the
/// release closes the window where a request that had seen neither could claim
/// the prefix and repeat the read.
pub(crate) async fn discover_and_cache(
    gate: &DiscoveryGate,
    config: &NodeConfig,
    db: &SqlitePool,
    prefix: &str,
    auth: Option<WorkflowAuth>,
    party_credentials: &[PartyCredentials],
    opts: PartyReadOpts,
) -> Discovery {
    // A targeted read answers a different question from the cached snapshot,
    // so it neither claims the prefix nor writes the cache: single-flighting
    // the two together would hand a windowed request the whole cached list.
    // It still takes a permit, because it is a Canton read, and a heavier one
    // when it pulls contracts.
    if !opts.is_cacheable() {
        let Ok(Ok(_permit)) = tokio::time::timeout(
            SINGLE_FLIGHT_WAIT,
            Arc::clone(&gate.permits).acquire_owned(),
        )
        .await
        else {
            return Discovery::AtCapacity;
        };

        return match budgeted_fetch(config, db, prefix, auth, party_credentials, opts).await {
            Ok(response) => {
                // A window that came back with parties proves the whole list is
                // not empty, so it clears a standing mark even though it writes
                // no cache and signals no completion. Otherwise the plain call
                // keeps answering empty for the rest of the TTL while this very
                // read held the evidence that the mark was stale.
                //
                // Guarded on non-empty because an offset past the end says
                // nothing about the whole list, and `record_discovery` would
                // read it as an empty result and mark the prefix.
                if !response.parties.is_empty() {
                    record_discovery(&gate.completed, prefix, &response.parties).await;
                }
                Discovery::Done(response)
            }
            Err(e) => Discovery::Failed(e),
        };
    }

    if gate.claims.read().await.contains(prefix) {
        return Discovery::InFlight;
    }

    // Noted before queuing for capacity. A discovery that runs and finishes
    // while this one waits leaves no claim behind, so the claim check alone
    // would let the second caller repeat the whole read.
    let generation_at_entry = gate
        .generations
        .read()
        .await
        .get(prefix)
        .map(|(generation, _)| *generation);

    let Ok(Ok(permit)) = tokio::time::timeout(
        SINGLE_FLIGHT_WAIT,
        Arc::clone(&gate.permits).acquire_owned(),
    )
    .await
    else {
        return Discovery::AtCapacity;
    };

    let generation_now = gate
        .generations
        .read()
        .await
        .get(prefix)
        .map(|(generation, _)| *generation);
    if discovery_completed_since(generation_at_entry, generation_now) {
        drop(permit);
        return Discovery::Superseded;
    }

    // The read above was not a claim, so settle it under the write lock.
    if !gate.claims.write().await.insert(prefix.to_string()) {
        drop(permit);
        return Discovery::InFlight;
    }
    let _permit = permit;

    let fetched = budgeted_fetch(config, db, prefix, auth, party_credentials, opts).await;

    let outcome = match fetched {
        Ok(response) => {
            record_discovery(&gate.completed, prefix, &response.parties).await;

            // Read before the store below deletes the parties this discovery
            // did not return from the cache.
            let returned: Vec<String> = response
                .parties
                .iter()
                .map(|party| party.party_id.to_string())
                .collect();
            let unreturned = unreturned_parties(
                db,
                party_credentials,
                config.validator_operator_party.as_ref(),
                prefix,
                &returned,
            )
            .await;

            match store_parties_to_db(db, prefix, &response.parties).await {
                Ok(()) => {
                    // Signalled last, while the claim still stands, so a
                    // waiter that sees the count move knows the mark and the
                    // rows are both in place. A count, not a timestamp:
                    // `updated_at` has one-second resolution, so a discovery
                    // finishing inside the same second was indistinguishable
                    // from one that never ran.
                    let now = now_secs();
                    let mut generations = gate.generations.write().await;
                    if !generations.contains_key(prefix) {
                        prune_and_cap(&mut generations, now, |(_, at)| *at);
                    }
                    let entry = generations.entry(prefix.to_string()).or_default();
                    *entry = (entry.0 + 1, now);
                    drop(generations);
                }
                // The rows a waiter would read are still the ones from before
                // this discovery, so completion must not be signalled. This
                // caller keeps its own live result.
                Err(e) => {
                    tracing::warn!("Failed to cache parties for prefix '{prefix}': {e}");
                }
            }

            former_parties::reconcile(config, db, &unreturned, &returned).await;

            Discovery::Done(response)
        }
        Err(e) => Discovery::Failed(e),
    };

    gate.claims.write().await.remove(prefix);
    outcome
}

/// The parties this node knew under `prefix` that a discovery did not return.
///
/// Best effort: a failed read only delays noticing a removal until the next
/// discovery.
async fn unreturned_parties(
    db: &SqlitePool,
    party_credentials: &[PartyCredentials],
    validator_operator_party: Option<&CantonId>,
    prefix: &str,
    returned: &[String],
) -> Vec<String> {
    let (runs, cached) = match (
        db.get_visible_workflow_runs().await,
        db.get_dec_parties_by_prefix("").await,
    ) {
        (Ok(runs), Ok(cached)) => (runs, cached),
        (Err(e), _) | (_, Err(e)) => {
            tracing::warn!("Could not list the known parties to check for removals: {e}");
            return Vec::new();
        }
    };
    let returned: HashSet<&String> = returned.iter().collect();
    known_party_filters(
        party_credentials,
        validator_operator_party,
        runs.into_iter()
            .filter_map(|run| run.dec_party_id.map(|party_id| party_id.to_string())),
        cached.into_iter().map(|party| party.party_id),
        Some(prefix).filter(|prefix| !prefix.is_empty()),
    )
    .into_iter()
    .filter(|party_id| !returned.contains(party_id))
    .collect()
}

/// One discovery under [`DISCOVERY_BUDGET`].
async fn budgeted_fetch(
    config: &NodeConfig,
    db: &SqlitePool,
    prefix: &str,
    auth: Option<WorkflowAuth>,
    party_credentials: &[PartyCredentials],
    opts: PartyReadOpts,
) -> Result<DecentralizedPartiesResponse> {
    match tokio::time::timeout(
        DISCOVERY_BUDGET,
        fetch_decentralized_parties(
            config,
            db,
            Some(prefix).filter(|prefix| !prefix.is_empty()),
            auth,
            party_credentials,
            opts,
        ),
    )
    .await
    {
        Ok(fetched) => fetched,
        Err(_) => anyhow::bail!(
            "discovery for prefix '{prefix}' exceeded {secs}s",
            secs = DISCOVERY_BUDGET.as_secs()
        ),
    }
}
#[cfg(test)]
mod tests {
    use tokio::sync::RwLock;

    use super::{super::test_support::a_party, *};

    /// A party this node does not belong to leaves no cached rows, so without
    /// a record of the completed discovery every request re-ran it. The
    /// boundary matters: the TTL second itself still answers from the record.
    #[test]
    fn a_completed_discovery_answers_until_the_ttl_passes() {
        let now = 1_000_000;

        assert!(!is_within_ttl(None, now));
        assert!(is_within_ttl(Some(now), now));
        assert!(is_within_ttl(Some(now - PARTIES_CACHE_TTL_SECS), now));
        assert!(!is_within_ttl(Some(now - PARTIES_CACHE_TTL_SECS - 1), now));
    }

    /// A backwards clock jump must not pin the answer. A negative age would
    /// satisfy a plain `<= TTL` and hold the record forever.
    #[test]
    fn a_timestamp_from_the_future_is_not_fresh() {
        let now = 1_000_000;

        assert!(!is_within_ttl(Some(now + 1), now));
        assert!(!is_within_ttl(Some(now + 86_400), now));
    }

    /// Only an empty result is recorded. A non-empty one is served from
    /// `dec_parties`, and recording it would make a request that arrives
    /// before that write answer empty for the whole TTL.
    #[tokio::test]
    async fn only_an_empty_discovery_is_recorded() -> anyhow::Result<()> {
        let completed = Arc::new(RwLock::new(HashMap::new()));

        record_discovery(&completed, "cbtc", &[a_party()?]).await;
        assert!(
            completed.read().await.is_empty(),
            "a non-empty discovery must not be recorded"
        );

        record_discovery(&completed, "cbtc", &[]).await;
        assert!(completed.read().await.contains_key("cbtc"));
        Ok(())
    }

    /// Eviction must never read as completion. A waiter holding a count for a
    /// prefix that has since been evicted would otherwise take the missing
    /// entry as proof that fresh rows had been written.
    #[test]
    fn an_evicted_count_is_not_a_completed_discovery() {
        assert!(discovery_completed_since(None, Some(1)));
        assert!(discovery_completed_since(Some(4), Some(5)));

        assert!(!discovery_completed_since(None, None));
        assert!(!discovery_completed_since(Some(4), None), "eviction");
        assert!(!discovery_completed_since(Some(4), Some(4)));
        assert!(
            !discovery_completed_since(Some(9), Some(1)),
            "a lower count means the prefix was evicted and started over"
        );
    }

    /// A discovery that finds parties has to clear a standing empty mark. If
    /// the cache write then fails, the next request must retry rather than
    /// read the old mark as "this prefix has no parties".
    #[tokio::test]
    async fn a_non_empty_discovery_clears_a_standing_mark() -> anyhow::Result<()> {
        let completed = Arc::new(RwLock::new(HashMap::new()));

        record_discovery(&completed, "cbtc", &[]).await;
        assert!(completed.read().await.contains_key("cbtc"));

        record_discovery(&completed, "cbtc", &[a_party()?]).await;
        assert!(
            !completed.read().await.contains_key("cbtc"),
            "a found party must invalidate the empty mark"
        );
        Ok(())
    }

    /// Eviction has to take the oldest entry, not the whole map: `prefix` is
    /// request-controlled, so clearing would let a stream of unique prefixes
    /// flush the entry a real prefix depends on.
    #[tokio::test]
    async fn a_full_map_evicts_the_oldest_entry() {
        let completed = Arc::new(RwLock::new(HashMap::new()));
        let now = now_secs();

        // All fresh, so nothing is dropped for age. The oldest is the one
        // eviction must choose.
        for index in 0..MAX_TRACKED_PREFIXES {
            completed
                .write()
                .await
                .insert(format!("prefix-{index}"), now - (index as i64 % 5));
        }
        completed
            .write()
            .await
            .insert("oldest".to_string(), now - 6);

        record_discovery(&completed, "newcomer", &[]).await;

        let seen = completed.read().await;
        assert!(!seen.contains_key("oldest"), "the oldest entry survived");
        assert!(seen.contains_key("newcomer"));
        assert!(seen.len() > 1, "the map was cleared instead of trimmed");
        drop(seen);

        // Re-stamping a tracked prefix does not grow the map, so it must not
        // evict anyone. Repeated refreshes of one prefix would drain the rest.
        let before: Vec<String> = completed.read().await.keys().cloned().collect();
        record_discovery(&completed, "newcomer", &[]).await;
        let after: Vec<String> = completed.read().await.keys().cloned().collect();
        assert_eq!(
            before.len(),
            after.len(),
            "re-stamping an existing prefix evicted another"
        );
    }

    /// `prefix` comes from the request, so the map has to stay bounded whoever
    /// is asking.
    #[tokio::test]
    async fn recording_drops_expired_entries_and_stays_bounded() {
        let completed = Arc::new(RwLock::new(HashMap::new()));
        let stale = now_secs() - PARTIES_CACHE_TTL_SECS - 1;
        completed.write().await.insert("expired".to_string(), stale);

        record_discovery(&completed, "fresh", &[]).await;

        let seen = completed.read().await;
        assert!(!seen.contains_key("expired"), "expired entry survived");
        assert!(seen.contains_key("fresh"));
        drop(seen);

        for index in 0..MAX_TRACKED_PREFIXES + 1 {
            record_discovery(&completed, &format!("prefix-{index}"), &[]).await;
        }
        assert!(
            completed.read().await.len() <= MAX_TRACKED_PREFIXES,
            "the map grew past its cap"
        );
    }
}
