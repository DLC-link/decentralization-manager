//! A fixed-window request budget per tenant API key. Each key gets a number of
//! calls per minute, and its budget resets when its minute ends. The state is
//! in memory, so a restart gives every key a fresh budget.
use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

/// How long one budget lasts.
pub const WINDOW: Duration = Duration::from_secs(60);

/// Request counts, keyed by the index of the key in `DECPM_TENANT_API_KEYS`.
#[derive(Default)]
pub struct TenantRateLimiter(Mutex<HashMap<usize, Window>>);

struct Window {
    started: Instant,
    count: u32,
}

/// Whether a request may go ahead.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    /// The key has spent its budget. `retry_after` is the time left in its
    /// window.
    Limited {
        retry_after: Duration,
    },
}

impl TenantRateLimiter {
    fn lock(&self) -> MutexGuard<'_, HashMap<usize, Window>> {
        // Nothing panics while holding the lock, so a poisoned map is intact.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Count one request for `key_index`, unless its budget of `limit`
    /// requests in this window is already spent. A refused request does not
    /// count, so a caller that keeps retrying does not extend its own wait.
    pub fn check(&self, key_index: usize, limit: u32, now: Instant) -> Verdict {
        let mut windows = self.lock();
        let window = windows.entry(key_index).or_insert(Window {
            started: now,
            count: 0,
        });
        let elapsed = now.saturating_duration_since(window.started);
        if elapsed >= WINDOW {
            window.started = now;
            window.count = 0;
        }
        if window.count >= limit {
            return Verdict::Limited {
                retry_after: WINDOW.saturating_sub(elapsed),
            };
        }
        window.count += 1;
        Verdict::Allowed
    }
}

/// The `Retry-After` value for a wait: whole seconds, rounded up, and never 0,
/// because a client told to wait 0 seconds retries at once and is refused
/// again.
pub fn retry_after_secs(wait: Duration) -> u64 {
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    secs.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn requests_under_the_limit_are_allowed() {
        let limiter = TenantRateLimiter::default();
        let t0 = Instant::now();
        for i in 0..3 {
            assert_eq!(
                limiter.check(0, 3, t0 + i * SECOND),
                Verdict::Allowed,
                "request {i}"
            );
        }
    }

    #[test]
    fn a_request_past_the_limit_waits_for_the_rest_of_the_window() {
        let limiter = TenantRateLimiter::default();
        let t0 = Instant::now();
        assert_eq!(limiter.check(0, 2, t0), Verdict::Allowed);
        assert_eq!(limiter.check(0, 2, t0 + SECOND), Verdict::Allowed);
        assert_eq!(
            limiter.check(0, 2, t0 + 20 * SECOND),
            Verdict::Limited {
                retry_after: 40 * SECOND
            }
        );
        // A refused request spends nothing, so the wait still ends with the
        // window.
        assert_eq!(
            limiter.check(0, 2, t0 + 50 * SECOND),
            Verdict::Limited {
                retry_after: 10 * SECOND
            }
        );
    }

    #[test]
    fn a_new_window_starts_after_a_minute() {
        let limiter = TenantRateLimiter::default();
        let t0 = Instant::now();
        assert_eq!(limiter.check(0, 1, t0), Verdict::Allowed);
        assert!(matches!(
            limiter.check(0, 1, t0 + WINDOW - SECOND),
            Verdict::Limited { .. }
        ));
        assert_eq!(limiter.check(0, 1, t0 + WINDOW), Verdict::Allowed);
        assert!(matches!(
            limiter.check(0, 1, t0 + WINDOW + SECOND),
            Verdict::Limited { .. }
        ));
    }

    #[test]
    fn each_key_has_its_own_budget() {
        let limiter = TenantRateLimiter::default();
        let t0 = Instant::now();
        assert_eq!(limiter.check(0, 1, t0), Verdict::Allowed);
        assert!(matches!(limiter.check(0, 1, t0), Verdict::Limited { .. }));
        assert_eq!(limiter.check(1, 1, t0), Verdict::Allowed);
    }

    #[test]
    fn retry_after_rounds_up_to_at_least_one_second() {
        assert_eq!(retry_after_secs(Duration::ZERO), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_after_secs(SECOND), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(1001)), 2);
        assert_eq!(retry_after_secs(Duration::from_millis(59_400)), 60);
    }
}
