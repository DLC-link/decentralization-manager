use crate::error::Result;

/// What the replay into a temporary store came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RebuildOutcome {
    /// The store's DNS and P2P heads are the synchronizer's.
    Rebuilt,
    /// The history was read, but replaying it did not rebuild the heads.
    ReplayFailed,
    /// The participant could not be reached, so nothing is known about the
    /// history. The step's own retry handles this.
    Unreachable,
}

/// How the coordinator signs a party's proposals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// Sign in the rebuilt temporary store; nothing is published.
    TemporaryStore,
    /// Sign with `Authorize` against the synchronizer store, which publishes
    /// each proposal as it signs it.
    Synchronizer,
    /// Stop the run: nothing is signed or published.
    Refuse,
    /// Return the error to the step, which retries it.
    Retry,
}

/// Decide how to sign, from the party's current namespace threshold and what
/// the replay came to.
///
/// The temporary store is always the first choice. Only a failed replay falls
/// back, and only above threshold 1: there the coordinator's signature alone
/// cannot meet the threshold, so a proposal published early stays pending. At
/// threshold 1 (or a malformed lower one) it would take effect at once, so
/// the run stops. An unreachable participant says nothing about the history,
/// so it never falls back.
pub(super) fn signing_route(head_threshold: i32, outcome: RebuildOutcome) -> Route {
    match outcome {
        RebuildOutcome::Rebuilt => Route::TemporaryStore,
        RebuildOutcome::Unreachable => Route::Retry,
        RebuildOutcome::ReplayFailed if head_threshold >= 2 => Route::Synchronizer,
        RebuildOutcome::ReplayFailed => Route::Refuse,
    }
}

/// Classify a replay result. An error is [`RebuildOutcome::Unreachable`] when
/// anything in its chain is a transport failure; every other error means the
/// replay itself failed.
pub(super) fn rebuild_outcome(result: &Result<usize>) -> RebuildOutcome {
    match result {
        Ok(_) => RebuildOutcome::Rebuilt,
        Err(e) if is_unreachable(e) => RebuildOutcome::Unreachable,
        Err(_) => RebuildOutcome::ReplayFailed,
    }
}

/// Whether an error comes from the connection to the participant rather than
/// from Canton refusing what it was asked.
pub(super) fn is_unreachable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<tonic::transport::Error>().is_some()
            || cause
                .downcast_ref::<tonic::Status>()
                .is_some_and(|status| is_transport_code(status.code()))
    })
}

/// gRPC codes a connection problem surfaces as. `Unknown` is among them
/// because tonic reports some broken connections that way, and an ambiguous
/// error must retry rather than fall back.
pub(super) fn is_transport_code(code: tonic::Code) -> bool {
    matches!(
        code,
        tonic::Code::Unavailable
            | tonic::Code::DeadlineExceeded
            | tonic::Code::Cancelled
            | tonic::Code::Unknown
            | tonic::Code::ResourceExhausted
            | tonic::Code::Aborted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- fallback --------------------------------------------------------

    /// A rebuilt store is used at every threshold. Falling back here would
    /// publish proposals the temporary store exists to hold back.
    #[test]
    fn a_rebuilt_store_is_always_used() {
        for threshold in [1, 2, 3] {
            assert_eq!(
                signing_route(threshold, RebuildOutcome::Rebuilt),
                Route::TemporaryStore,
                "threshold {threshold}"
            );
        }
    }

    /// At threshold 1 the coordinator alone authorizes the namespace change,
    /// so a proposal published early takes effect: a failed replay stops.
    #[test]
    fn a_failed_replay_at_threshold_one_refuses() {
        for threshold in [1, 0] {
            assert_eq!(
                signing_route(threshold, RebuildOutcome::ReplayFailed),
                Route::Refuse,
                "threshold {threshold}"
            );
        }
    }

    /// Above threshold 1 the coordinator alone cannot meet the threshold, so
    /// signing against the synchronizer, as before #448, is safe.
    #[test]
    fn a_failed_replay_above_threshold_one_falls_back() {
        for threshold in [2, 3, 7] {
            assert_eq!(
                signing_route(threshold, RebuildOutcome::ReplayFailed),
                Route::Synchronizer,
                "threshold {threshold}"
            );
        }
    }

    /// An unreachable participant says nothing about the history, so the
    /// step retries at every threshold and never falls back.
    #[test]
    fn an_unreachable_participant_never_falls_back() {
        for threshold in [1, 2, 3] {
            assert_eq!(
                signing_route(threshold, RebuildOutcome::Unreachable),
                Route::Retry,
                "threshold {threshold}"
            );
        }
    }

    #[test]
    fn transport_failures_read_as_unreachable_and_refusals_as_failed_replays() {
        let unreachable: [Result<usize>; 3] = [
            Err(tonic::Status::unavailable("connection refused").into()),
            Err(
                anyhow::Error::from(tonic::Status::deadline_exceeded("slow"))
                    .context("list the party's history"),
            ),
            Err(tonic::Status::unknown("h2 protocol error").into()),
        ];
        for result in &unreachable {
            assert_eq!(
                rebuild_outcome(result),
                RebuildOutcome::Unreachable,
                "{result:?}"
            );
        }

        let failed_replays: [Result<usize>; 3] = [
            Err(tonic::Status::invalid_argument("unknown mapping code").into()),
            Err(tonic::Status::failed_precondition("TOPOLOGY_SERIAL_MISMATCH").into()),
            Err(anyhow::anyhow!("the store holds DNS serial none")),
        ];
        for result in &failed_replays {
            assert_eq!(
                rebuild_outcome(result),
                RebuildOutcome::ReplayFailed,
                "{result:?}"
            );
        }

        assert_eq!(rebuild_outcome(&Ok(12)), RebuildOutcome::Rebuilt);
    }
}
