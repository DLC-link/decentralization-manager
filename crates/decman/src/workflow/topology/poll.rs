use std::{future::Future, time::Duration};

use crate::{
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
};

/// What one attempt of a topology poll found.
pub enum Polled<T> {
    /// The condition holds, and the poll returns `T`.
    Ready(T),
    /// Not yet. The text says what is still missing, for the debug line
    /// logged before the next attempt.
    Pending(String),
}

/// The log line and the error of one topology poll.
pub struct PollText {
    /// Logged at info on success, followed by " after N attempt(s)".
    pub found: String,
    /// The error after the last attempt, followed by " after N attempts".
    pub failed: String,
}

/// Run `attempt` until it is [`Polled::Ready`], within the topology retry
/// budget ([`topology_retry_max_attempts`] attempts,
/// [`topology_retry_delay_secs`] apart).
///
/// An error from `attempt` ends the poll at once; only a pending answer is
/// retried. `attempt` returns an owned future (an `async move` block), so a
/// caller clones what one attempt needs, such as a gRPC client.
///
/// # Errors
/// Returns the first error `attempt` returns, or `text.failed` when the last
/// attempt is still pending.
pub async fn poll_until<T, Fut>(text: PollText, attempt: impl FnMut() -> Fut) -> Result<T>
where
    Fut: Future<Output = Result<Polled<T>>>,
{
    poll_until_with(
        topology_retry_max_attempts(),
        Duration::from_secs(topology_retry_delay_secs()),
        text,
        attempt,
    )
    .await
}

async fn poll_until_with<T, Fut>(
    max_attempts: usize,
    retry_delay: Duration,
    text: PollText,
    mut poll: impl FnMut() -> Fut,
) -> Result<T>
where
    Fut: Future<Output = Result<Polled<T>>>,
{
    for attempt in 1..=max_attempts {
        match poll().await? {
            Polled::Ready(value) => {
                tracing::info!("{found} after {attempt} attempt(s)", found = text.found);
                return Ok(value);
            }
            Polled::Pending(missing) => {
                if attempt < max_attempts {
                    tracing::debug!(
                        "{missing}, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
                    );
                    tokio::time::sleep(retry_delay).await;
                }
            }
        }
    }

    anyhow::bail!(
        "{failed} after {max_attempts} attempts",
        failed = text.failed
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> PollText {
        PollText {
            found: "DNS found in topology".to_string(),
            failed: "DNS did not appear in topology".to_string(),
        }
    }

    #[tokio::test]
    async fn returns_the_value_of_the_first_ready_attempt() -> Result<()> {
        let mut calls = 0;
        let value = poll_until_with(3, Duration::ZERO, text(), || {
            calls += 1;
            async { Ok(Polled::Ready(7)) }
        })
        .await?;

        assert_eq!(value, 7);
        assert_eq!(calls, 1);
        Ok(())
    }

    #[tokio::test]
    async fn retries_a_pending_attempt_until_it_is_ready() -> Result<()> {
        let mut calls = 0;
        let value = poll_until_with(5, Duration::ZERO, text(), || {
            calls += 1;
            let call = calls;
            async move {
                Ok(if call < 3 {
                    Polled::Pending("DNS not yet in topology".to_string())
                } else {
                    Polled::Ready(call)
                })
            }
        })
        .await?;

        assert_eq!(value, 3);
        assert_eq!(calls, 3);
        Ok(())
    }

    #[tokio::test]
    async fn fails_with_the_callers_text_once_every_attempt_is_pending() {
        let mut calls = 0;
        let result = poll_until_with(4, Duration::ZERO, text(), || {
            calls += 1;
            async { Ok(Polled::<()>::Pending("DNS not yet in topology".to_string())) }
        })
        .await;

        let Err(e) = result else {
            panic!("an exhausted poll must fail");
        };
        assert_eq!(
            e.to_string(),
            "DNS did not appear in topology after 4 attempts"
        );
        assert_eq!(calls, 4);
    }

    #[tokio::test]
    async fn stops_at_the_first_error_without_retrying() {
        let mut calls = 0;
        let result = poll_until_with(4, Duration::ZERO, text(), || {
            calls += 1;
            async { Err::<Polled<()>, _>(anyhow::anyhow!("ListPartyToParticipant failed")) }
        })
        .await;

        let Err(e) = result else {
            panic!("an attempt error must end the poll");
        };
        assert_eq!(e.to_string(), "ListPartyToParticipant failed");
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn sleeps_the_delay_between_two_attempts() {
        let delay = Duration::from_millis(50);
        let started = tokio::time::Instant::now();
        let _ = poll_until_with(2, delay, text(), || async {
            Ok(Polled::<()>::Pending("not yet".to_string()))
        })
        .await;

        assert!(started.elapsed() >= delay, "slept {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn does_not_sleep_after_the_last_attempt() {
        // One attempt with an hour's delay: a sleep after it would hang.
        let poll = poll_until_with(1, Duration::from_secs(3600), text(), || async {
            Ok(Polled::<()>::Pending("not yet".to_string()))
        });

        let finished = tokio::time::timeout(Duration::from_secs(5), poll).await;

        assert!(finished.is_ok(), "the poll slept after its last attempt");
    }
}
