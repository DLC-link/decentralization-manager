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

/// The two log calls of one topology poll.
///
/// `poll_log!` builds them at the call site, so each line keeps the
/// caller's module as its tracing target.
pub struct PollLog {
    /// Logs at info that the poll found its condition: the found text and
    /// the attempt number.
    pub ready: fn(&str, usize),
    /// Logs at debug that an attempt is still pending: the missing text, the
    /// attempt number, the attempt budget and the delay before the next try.
    pub pending: fn(&str, usize, usize, Duration),
}

/// Build the [`PollLog`] of a topology poll at the call site.
macro_rules! poll_log {
    () => {
        $crate::workflow::topology::PollLog {
            ready: |found, attempt| {
                ::tracing::info!("{found} after {attempt} attempt(s)");
            },
            pending: |missing, attempt, max_attempts, retry_delay| {
                ::tracing::debug!(
                    "{missing}, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
                );
            },
        }
    };
}

/// Run `attempt` until it is [`Polled::Ready`], within the topology retry
/// budget ([`topology_retry_max_attempts`] attempts,
/// [`topology_retry_delay_secs`] apart). Expands to a future of
/// `Result<T>`.
///
/// An error from `attempt` ends the poll at once; only a pending answer is
/// retried. `attempt` returns an owned future (an `async move` block), so a
/// caller clones what one attempt needs, such as a gRPC client.
///
/// A macro, not a function, so that the two log lines expand at the call
/// site and keep the caller's module as their tracing target.
///
/// The future fails with the first error `attempt` returns, or with
/// `text.failed` when the last attempt is still pending.
macro_rules! poll_until {
    ($text:expr, $attempt:expr $(,)?) => {
        $crate::workflow::topology::poll_until_logged(
            $crate::workflow::topology::poll_log!(),
            $text,
            $attempt,
        )
    };
}

pub(crate) use {poll_log, poll_until};

/// The function behind `poll_until!`. Call the macro instead: `log` decides
/// the tracing target of the poll's log lines.
///
/// # Errors
/// Returns the first error `attempt` returns, or `text.failed` when the last
/// attempt is still pending.
pub async fn poll_until_logged<T, Fut>(
    log: PollLog,
    text: PollText,
    attempt: impl FnMut() -> Fut,
) -> Result<T>
where
    Fut: Future<Output = Result<Polled<T>>>,
{
    poll_until_with(
        topology_retry_max_attempts(),
        Duration::from_secs(topology_retry_delay_secs()),
        log,
        text,
        attempt,
    )
    .await
}

async fn poll_until_with<T, Fut>(
    max_attempts: usize,
    retry_delay: Duration,
    log: PollLog,
    text: PollText,
    mut poll: impl FnMut() -> Fut,
) -> Result<T>
where
    Fut: Future<Output = Result<Polled<T>>>,
{
    for attempt in 1..=max_attempts {
        match poll().await? {
            Polled::Ready(value) => {
                (log.ready)(&text.found, attempt);
                return Ok(value);
            }
            Polled::Pending(missing) => {
                if attempt < max_attempts {
                    (log.pending)(&missing, attempt, max_attempts, retry_delay);
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
    use std::{
        fmt,
        sync::{Arc, Mutex},
    };

    use tokio::time::Instant;
    use tracing::{
        Event, Level, Subscriber,
        field::{Field, Visit},
        instrument::WithSubscriber,
    };
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

    use super::*;

    /// One log line as a subscriber saw it.
    #[derive(Debug, PartialEq)]
    struct Line {
        target: String,
        level: Level,
        fields: Vec<String>,
        message: String,
    }

    /// A layer that records the target, level, field names and message of
    /// every event.
    struct Capture(Arc<Mutex<Vec<Line>>>);

    impl<S: Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            let metadata = event.metadata();
            self.0.lock().expect("capture lock").push(Line {
                target: metadata.target().to_string(),
                level: *metadata.level(),
                fields: fields.names,
                message: fields.message,
            });
        }
    }

    #[derive(Default)]
    struct Fields {
        names: Vec<String>,
        message: String,
    }

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.names.push(field.name().to_string());
            if field.name() == "message" {
                self.message = format!("{value:?}");
            }
        }
    }

    fn text() -> PollText {
        PollText {
            found: "DNS found in topology".to_string(),
            failed: "DNS did not appear in topology".to_string(),
        }
    }

    #[tokio::test]
    async fn returns_the_value_of_the_first_ready_attempt() -> Result<()> {
        let mut calls = 0;
        let value = poll_until_with(3, Duration::ZERO, poll_log!(), text(), || {
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
        let value = poll_until_with(5, Duration::ZERO, poll_log!(), text(), || {
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
        let result = poll_until_with(4, Duration::ZERO, poll_log!(), text(), || {
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
        let result = poll_until_with(4, Duration::ZERO, poll_log!(), text(), || {
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
        let started = Instant::now();
        let _ = poll_until_with(2, delay, poll_log!(), text(), || async {
            Ok(Polled::<()>::Pending("not yet".to_string()))
        })
        .await;

        assert!(started.elapsed() >= delay, "slept {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn does_not_sleep_after_the_last_attempt() {
        // One attempt with an hour's delay: a sleep after it would hang.
        let poll = poll_until_with(
            1,
            Duration::from_secs(3600),
            poll_log!(),
            text(),
            || async { Ok(Polled::<()>::Pending("not yet".to_string())) },
        );

        let finished = tokio::time::timeout(Duration::from_secs(5), poll).await;

        assert!(finished.is_ok(), "the poll slept after its last attempt");
    }

    /// A caller's poll logs at the caller's module, not at this module's
    /// parent, where the loop runs. Each line carries only a message, as the
    /// hand-written loops did.
    #[tokio::test]
    async fn logs_both_lines_at_the_callers_target() -> Result<()> {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Capture(Arc::clone(&lines)));
        let mut calls = 0;

        poll_until_with(3, Duration::ZERO, poll_log!(), text(), || {
            calls += 1;
            let call = calls;
            async move {
                Ok(if call < 2 {
                    Polled::Pending("DNS not yet in topology".to_string())
                } else {
                    Polled::Ready(())
                })
            }
        })
        .with_subscriber(subscriber)
        .await?;

        let line = |level, message: &str| Line {
            target: module_path!().to_string(),
            level,
            fields: vec!["message".to_string()],
            message: message.to_string(),
        };
        assert_eq!(
            *lines.lock().expect("capture lock"),
            vec![
                line(
                    Level::DEBUG,
                    "DNS not yet in topology, attempt 1/3, retrying in 0ns..."
                ),
                line(Level::INFO, "DNS found in topology after 2 attempt(s)"),
            ]
        );
        Ok(())
    }
}
