use futures::StreamExt;

pub use crate::canton::{
    clients::{
        MAX_GRPC_MESSAGE_SIZE, create_command_client, create_event_query_client,
        create_party_client, create_state_client, create_submission_client, create_update_client,
        create_user_client, get_participant_id, resolve_participant_id,
    },
    fingerprint::{MULTIHASH_SHA256_PREFIX, compute_fingerprint},
    framing::{
        decode_files, decode_length_prefixed, decode_versioned, encode_files,
        encode_length_prefixed, encode_length_prefixed_message, read_first_message_from_bytes,
    },
    synchronizer::{
        extract_synchronizer_fingerprint, fetch_synchronizer_id,
        forget_synchronizer_id_on_unknown_store, get_synchronizer_id,
    },
};

/// Whether version `v` is at least `min`, by semver precedence: a
/// pre-release precedes its own release (`1.4.3-rc1` < `1.4.3`) but outranks
/// any lower core (`1.4.3-rc1` > `0.1.9`), and build metadata is ignored.
/// Both inputs must be full `x.y.z` semver — the only real input is a peer's
/// `CARGO_PKG_VERSION`, which Cargo guarantees is. Returns `false` for
/// anything that doesn't parse — for version gating, "can't parse" must mean
/// "can't verify", never "assume compatible".
pub fn version_at_least(v: &str, min: &str) -> bool {
    let (Ok(v), Ok(min)) = (
        semver::Version::parse(v.trim()),
        semver::Version::parse(min.trim()),
    ) else {
        return false;
    };
    v >= min
}

/// Await `tasks` with at most `limit` of them in flight, in input order,
/// keeping the values that succeeded.
///
/// The bound is the point. Awaiting a whole batch together — `join_all` over
/// one future per item — makes peak memory scale with the batch, because every
/// future's buffers and results are alive at once. Reading one party per future
/// that way OOMKilled a node hosting 213 of them (#415). Here the peak tracks
/// `limit`.
///
/// Order is preserved (`buffered`, not `buffer_unordered`) so a caller's result
/// list does not reshuffle between calls. A failed future is dropped, so the
/// caller decides nothing per item; log the cause inside the future if it
/// matters. `limit` of 0 is treated as 1, so a caller can never deadlock on a
/// misconfigured bound.
pub async fn bounded_ordered<T, F>(limit: usize, tasks: impl Iterator<Item = F>) -> Vec<T>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    futures::stream::iter(tasks)
        .buffered(limit.max(1))
        .filter_map(|r| async move { r.ok() })
        .collect()
        .await
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    /// A future that records how many of its peers run at the same time.
    async fn tracked(
        n: usize,
        live: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    ) -> anyhow::Result<usize> {
        let now = live.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
        // Yield enough times that every future the bound allows has started
        // before the first one finishes; without this the executor could run
        // them one after another and the test would pass for the wrong reason.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        live.fetch_sub(1, Ordering::SeqCst);
        Ok(n)
    }

    #[tokio::test]
    async fn bounded_ordered_never_exceeds_the_limit() {
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let got = bounded_ordered(4, (0..40).map(|n| tracked(n, live.clone(), peak.clone()))).await;

        assert_eq!(got.len(), 40);
        assert!(
            peak.load(Ordering::SeqCst) <= 4,
            "ran {} at once against a limit of 4",
            peak.load(Ordering::SeqCst)
        );
        // And it really did overlap — a bound that serialised everything would
        // also satisfy the assertion above while losing all the concurrency.
        assert!(
            peak.load(Ordering::SeqCst) > 1,
            "never ran two at once, so the bound serialised the batch"
        );
    }

    #[tokio::test]
    async fn bounded_ordered_keeps_input_order() {
        // Reverse the completion order: the first future waits the longest, so
        // an unordered bound would return it last.
        let got = bounded_ordered(
            8,
            (0..8usize).map(|n| async move {
                for _ in 0..(8 - n) {
                    tokio::task::yield_now().await;
                }
                Ok(n)
            }),
        )
        .await;
        assert_eq!(got, (0..8).collect::<Vec<usize>>());
    }

    #[tokio::test]
    async fn bounded_ordered_drops_the_failures_and_keeps_the_rest() {
        let got = bounded_ordered(
            3,
            (0..6usize).map(|n| async move {
                if n % 2 == 0 {
                    Ok(n)
                } else {
                    Err(anyhow::anyhow!("no"))
                }
            }),
        )
        .await;
        assert_eq!(got, vec![0, 2, 4]);
    }

    #[tokio::test]
    async fn bounded_ordered_treats_a_zero_limit_as_one() {
        // `buffered(0)` never polls anything, which would hang the caller.
        let got = bounded_ordered(0, (0..3usize).map(|n| async move { Ok(n) })).await;
        assert_eq!(got, vec![0, 1, 2]);
    }

    #[test]
    fn version_at_least_compares_numerically() {
        assert!(version_at_least("0.1.9", "0.1.9"));
        assert!(version_at_least("0.1.10", "0.1.9"));
        assert!(version_at_least("0.2.0", "0.1.9"));
        assert!(version_at_least("1.0.0", "0.1.9"));
        assert!(!version_at_least("0.1.8", "0.1.9"));
        // Semver precedence: a pre-release precedes its own release but
        // outranks any lower core.
        assert!(!version_at_least("0.1.9-rc1", "0.1.9"));
        assert!(version_at_least("0.1.10-rc1", "0.1.9"));
        assert!(version_at_least("1.4.3-rc1", "0.1.9"));
        // Build metadata is ignored for precedence.
        assert!(version_at_least("1.4.3+build5", "0.1.9"));
        assert!(version_at_least("1.0.0-rc1+build2", "0.1.9"));
        // Unparseable — including partial versions, which a real
        // CARGO_PKG_VERSION can never be — must never pass the gate.
        assert!(!version_at_least("", "0.1.9"));
        assert!(!version_at_least("1.0", "0.1.9"));
        assert!(!version_at_least("-rc1", "0.1.9"));
        assert!(!version_at_least("abc", "0.1.9"));
    }
}
