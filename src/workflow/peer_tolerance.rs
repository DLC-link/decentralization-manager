use std::time::{Duration, Instant};

use crate::server::duration_from_env_or;

const PEER_CONNECTION_CLASS_DEADLINE_DEFAULT: Duration = Duration::from_secs(180);

/// Layer 2 connection-class budget. Read at first call via OnceLock; override
/// with `DPM_PEER_CONNECTION_CLASS_DEADLINE_SECS`. Default 180s.
pub fn peer_connection_class_deadline() -> Duration {
    static V: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        duration_from_env_or(
            "DPM_PEER_CONNECTION_CLASS_DEADLINE_SECS",
            PEER_CONNECTION_CLASS_DEADLINE_DEFAULT,
        )
    })
}

/// Partition of step errors used by the peer's outer poll loop. Connection-class
/// errors are absorbed by `ConnectionClassDeadline` (180s wall-clock budget);
/// real errors still bump the existing 3-strike counter against
/// `MAX_CONSECUTIVE_STEP_FAILURES`.
#[derive(Debug, thiserror::Error)]
pub enum PeerStepError {
    #[error("connection-class failure: {0}")]
    ConnectionClass(anyhow::Error),
    #[error(transparent)]
    Real(anyhow::Error),
}

/// Wall-clock budget for *continuous* connection-class failures.
///
/// `record_failure` returns true once the budget is exhausted (caller should
/// then treat the peer as aborted). `reset` is called on the first successful
/// round-trip after any sequence of failures.
#[derive(Debug)]
pub struct ConnectionClassDeadline {
    first_failure_at: Option<Instant>,
    budget: Duration,
}

impl ConnectionClassDeadline {
    pub fn new(budget: Duration) -> Self {
        Self {
            first_failure_at: None,
            budget,
        }
    }

    /// Returns true if the wall-clock budget has been exhausted.
    pub fn record_failure(&mut self, now: Instant) -> bool {
        match self.first_failure_at {
            None => {
                self.first_failure_at = Some(now);
                false
            }
            Some(since) => now.duration_since(since) > self.budget,
        }
    }

    /// Reset on first success.
    pub fn reset(&mut self) {
        self.first_failure_at = None;
    }
}

/// Classify a step error into `ConnectionClass` or `Real`.
///
/// Connection-class errors:
/// - `std::io::ErrorKind::ConnectionRefused | TimedOut | ConnectionAborted`
/// - `crate::noise::NoiseError::Io(io_err)` with the same kinds
/// - `reqwest::Error` with `is_connect()` / `is_timeout()` / `status() == 503`
///
/// Everything else → Real (fail-safe).
pub fn classify(err: anyhow::Error) -> PeerStepError {
    use std::io::ErrorKind;

    // Direct io::Error downcast.
    if let Some(io_err) = err.downcast_ref::<std::io::Error>()
        && matches!(
            io_err.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::TimedOut | ErrorKind::ConnectionAborted
        )
    {
        return PeerStepError::ConnectionClass(err);
    }

    // NoiseError wraps io::Error inside its Io variant.
    if let Some(crate::noise::NoiseError::Io(io_err)) =
        err.downcast_ref::<crate::noise::NoiseError>()
        && matches!(
            io_err.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::TimedOut | ErrorKind::ConnectionAborted
        )
    {
        return PeerStepError::ConnectionClass(err);
    }

    // reqwest::Error downcast — covers coordinator HTTP calls.
    if let Some(req_err) = err.downcast_ref::<reqwest::Error>()
        && (req_err.is_connect()
            || req_err.is_timeout()
            || req_err.status().is_some_and(|s| s.as_u16() == 503))
    {
        return PeerStepError::ConnectionClass(err);
    }

    PeerStepError::Real(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_resets_on_success() {
        let mut d = ConnectionClassDeadline::new(Duration::from_secs(180));
        let t0 = Instant::now();
        assert!(!d.record_failure(t0));
        assert!(d.first_failure_at.is_some());
        d.reset();
        assert!(d.first_failure_at.is_none());
    }

    #[test]
    fn deadline_expires_after_budget() {
        let mut d = ConnectionClassDeadline::new(Duration::from_millis(100));
        let t0 = Instant::now();
        assert!(!d.record_failure(t0));
        assert!(!d.record_failure(t0 + Duration::from_millis(50)));
        assert!(d.record_failure(t0 + Duration::from_millis(200)));
    }

    #[test]
    fn deadline_does_not_reset_between_failures() {
        let mut d = ConnectionClassDeadline::new(Duration::from_millis(100));
        let t0 = Instant::now();
        d.record_failure(t0);
        d.record_failure(t0 + Duration::from_millis(20));
        d.record_failure(t0 + Duration::from_millis(40));
        assert!(!d.record_failure(t0 + Duration::from_millis(60)));
    }

    #[test]
    fn classify_io_connection_refused_is_connection_class() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "x");
        let err: anyhow::Error = anyhow::Error::new(io_err);
        assert!(matches!(classify(err), PeerStepError::ConnectionClass(_)));
    }

    #[test]
    fn classify_io_timed_out_is_connection_class() {
        let io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "x");
        let err: anyhow::Error = anyhow::Error::new(io_err);
        assert!(matches!(classify(err), PeerStepError::ConnectionClass(_)));
    }

    #[test]
    fn classify_other_io_is_real() {
        let io_err = std::io::Error::new(std::io::ErrorKind::InvalidData, "x");
        let err: anyhow::Error = anyhow::Error::new(io_err);
        assert!(matches!(classify(err), PeerStepError::Real(_)));
    }

    #[test]
    fn classify_arbitrary_anyhow_is_real() {
        let err = anyhow::anyhow!("unrelated business-logic failure");
        assert!(matches!(classify(err), PeerStepError::Real(_)));
    }
}
