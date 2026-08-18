//! Failure diagnostics for polled `Then` probes.
//!
//! A probe returns `None` to mean "not observable yet, keep polling". That
//! makes every failure — a restart window, a 500, a typo in the probe's own
//! URL — look identical to the [`Scenario`](super::scenario::Scenario) runner,
//! which then reports nothing but "timed out after 60s".
//!
//! [`ProbeDiag`] is the side channel that closes the gap: probes record what
//! went wrong, the runner reads it back to annotate the timeout and to end the
//! step early when the request itself is the problem.
//!
//! # Requirement on probe response types
//!
//! [`Failure::Deserialize`] is classified [`Class::Fatal`] with no
//! discriminator, so **a type used by a polled probe must tolerate every
//! transitional shape the endpoint can emit while the SUT settles.** The
//! shared DTOs satisfy this today by making optional fields `Option` with
//! `#[serde(default)]`, so a partially-populated body still deserializes.
//!
//! A probe type that instead requires a field the endpoint fills in only once
//! some background work completes would be failed here at roughly
//! `FATAL_STREAK × POLL_INTERVAL` — about six seconds — where before this
//! module it would have polled and eventually passed. Nothing enforces the
//! requirement; it is on whoever adds the next probe type.

use std::sync::{Arc, Mutex, MutexGuard};

use reqwest::StatusCode;
use tracing::{debug, warn};

/// Consecutive identical [`Class::Fatal`] failures that end a polled step early
/// instead of letting it run to its deadline.
pub const FATAL_STREAK: u32 = 3;

/// Longest failure body echoed into a log line or error message.
const BODY_SNIPPET: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// The SUT may still get there: connection refused during a restart, a
    /// 5xx, a token blip, or a handler answering "that resource does not exist
    /// yet" for something a later step creates.
    Transient,
    /// The request will stay wrong however long it is retried: no such route,
    /// a malformed query, a success body the probe cannot parse.
    Fatal,
}

/// A failed probe request, before it is classified.
pub enum Failure<'a> {
    /// No response at all — connection refused, timeout, token acquisition.
    Transport,
    /// A non-2xx response and its body.
    Status(StatusCode, &'a str),
    /// A 2xx response whose body did not deserialize into the probe's type.
    Deserialize,
}

impl Failure<'_> {
    pub fn class(&self) -> Class {
        match self {
            Failure::Transport => Class::Transient,
            Failure::Deserialize => Class::Fatal,
            Failure::Status(status, body) => match status.as_u16() {
                408 | 429 | 401 | 403 => Class::Transient,
                // A handler-issued 404 carries a JSON error object and means
                // "not created yet" — `GET /v0/tenant/{party}/status` answers
                // that way while onboarding is still in flight. An unrouted
                // path falls through to the frontend catch-all instead, whose
                // body is not JSON: that one is a typo and never resolves.
                //
                // This split therefore depends on the frontend catch-all
                // serving a NON-JSON body (`server::assets`). If it ever
                // answers 404 with JSON, every unrouted path reads as
                // transient again and a probe-path typo goes back to costing
                // a full silent timeout. Degrades gracefully, but check here
                // when changing what the catch-all returns.
                404 => {
                    if serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(body)
                        .is_ok()
                    {
                        Class::Transient
                    } else {
                        Class::Fatal
                    }
                }
                _ if status.is_client_error() => Class::Fatal,
                _ => Class::Transient,
            },
        }
    }
}

pub fn snippet(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= BODY_SNIPPET {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(BODY_SNIPPET).collect();
    format!("{head}…")
}

#[derive(Debug, Default)]
struct Diag {
    last_error: Option<String>,
    key: Option<String>,
    class: Option<Class>,
    repeats: u32,
}

/// Shared record of the most recent probe failure, cleared by the runner at
/// the start of every polled step.
#[derive(Debug, Clone, Default)]
pub struct ProbeDiag(Arc<Mutex<Diag>>);

impl ProbeDiag {
    fn guard(&self) -> MutexGuard<'_, Diag> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record a failure against `key` (a request identity, e.g. `GET :8081/x`).
    /// Repeats of the same key are counted; the first is logged at `warn` and
    /// the rest at `debug` so a 60s poll cannot bury the run's real warnings.
    pub fn record(&self, key: &str, class: Class, message: String) {
        let mut d = self.guard();
        if d.key.as_deref() == Some(key) && d.class == Some(class) {
            d.repeats += 1;
        } else {
            d.key = Some(key.to_string());
            d.class = Some(class);
            d.repeats = 1;
        }
        if d.repeats == 1 {
            warn!("probe failure ({class:?}): {message}");
        } else {
            debug!("probe failure ({class:?}, x{}): {message}", d.repeats);
        }
        d.last_error = Some(message);
    }

    /// Keep polling on `Err`, but record why first. For probe reads that are
    /// not a single HTTP request — a SQLite count, a multi-call ledger query —
    /// where the failure cannot be classified, so it never ends the step early.
    pub fn ok<T>(&self, key: &str, result: anyhow::Result<T>) -> Option<T> {
        match result {
            Ok(v) => {
                self.record_ok(key);
                Some(v)
            }
            Err(e) => {
                self.record(key, Class::Transient, format!("{key}: {e:#}"));
                None
            }
        }
    }

    /// Note that `key` succeeded, ending any streak it had accumulated.
    pub fn record_ok(&self, key: &str) {
        let mut d = self.guard();
        if d.key.as_deref() == Some(key) {
            *d = Diag::default();
        }
    }

    /// The failure a polled step should stop on: the same fatal request
    /// repeated [`FATAL_STREAK`] times.
    pub fn fatal(&self) -> Option<String> {
        let d = self.guard();
        (d.class == Some(Class::Fatal) && d.repeats >= FATAL_STREAK)
            .then(|| d.last_error.clone())
            .flatten()
    }

    pub fn last_error(&self) -> Option<String> {
        self.guard().last_error.clone()
    }

    pub fn reset(&self) {
        *self.guard() = Diag::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_errors_and_transport_are_transient() {
        assert_eq!(Failure::Transport.class(), Class::Transient);
        assert_eq!(
            Failure::Status(StatusCode::INTERNAL_SERVER_ERROR, "boom").class(),
            Class::Transient
        );
        assert_eq!(
            Failure::Status(StatusCode::SERVICE_UNAVAILABLE, "").class(),
            Class::Transient
        );
        assert_eq!(
            Failure::Status(StatusCode::UNAUTHORIZED, "expired").class(),
            Class::Transient
        );
    }

    #[test]
    fn client_errors_and_unparseable_bodies_are_fatal() {
        assert_eq!(
            Failure::Status(StatusCode::UNPROCESSABLE_ENTITY, "bad field").class(),
            Class::Fatal
        );
        assert_eq!(
            Failure::Status(StatusCode::METHOD_NOT_ALLOWED, "").class(),
            Class::Fatal
        );
        assert_eq!(Failure::Deserialize.class(), Class::Fatal);
    }

    #[test]
    fn handler_issued_404_retries_but_unrouted_404_is_fatal() {
        assert_eq!(
            Failure::Status(StatusCode::NOT_FOUND, r#"{"error":"party not onboarded"}"#).class(),
            Class::Transient
        );
        assert_eq!(
            Failure::Status(StatusCode::NOT_FOUND, "404 Not Found").class(),
            Class::Fatal
        );
        assert_eq!(
            Failure::Status(StatusCode::NOT_FOUND, "").class(),
            Class::Fatal
        );
        assert_eq!(
            Failure::Status(StatusCode::NOT_FOUND, "<!doctype html><title>app</title>").class(),
            Class::Fatal
        );
    }

    #[test]
    fn fatal_reported_only_after_the_streak() {
        let diag = ProbeDiag::default();
        for _ in 0..FATAL_STREAK - 1 {
            diag.record("GET :8081/typo", Class::Fatal, "404".to_string());
            assert_eq!(diag.fatal(), None);
        }
        diag.record("GET :8081/typo", Class::Fatal, "404".to_string());
        assert_eq!(diag.fatal().as_deref(), Some("404"));
    }

    #[test]
    fn a_different_request_restarts_the_streak() {
        let diag = ProbeDiag::default();
        for _ in 0..FATAL_STREAK {
            diag.record("GET :8081/a", Class::Fatal, "404 a".to_string());
        }
        diag.record("GET :8081/b", Class::Fatal, "404 b".to_string());
        assert_eq!(diag.fatal(), None);
        assert_eq!(diag.last_error().as_deref(), Some("404 b"));
    }

    #[test]
    fn transient_repeats_never_become_fatal() {
        let diag = ProbeDiag::default();
        for _ in 0..FATAL_STREAK * 3 {
            diag.record(
                "GET :8081/x",
                Class::Transient,
                "connection refused".to_string(),
            );
        }
        assert_eq!(diag.fatal(), None);
        assert_eq!(diag.last_error().as_deref(), Some("connection refused"));
    }

    #[test]
    fn success_clears_its_own_streak_only() {
        let diag = ProbeDiag::default();
        for _ in 0..FATAL_STREAK {
            diag.record("GET :8081/a", Class::Fatal, "404 a".to_string());
        }
        diag.record_ok("GET :8081/b");
        assert_eq!(diag.fatal().as_deref(), Some("404 a"));
        diag.record_ok("GET :8081/a");
        assert_eq!(diag.fatal(), None);
        assert_eq!(diag.last_error(), None);
    }

    #[test]
    fn reset_clears_everything() {
        let diag = ProbeDiag::default();
        for _ in 0..FATAL_STREAK {
            diag.record("GET :8081/a", Class::Fatal, "404 a".to_string());
        }
        diag.reset();
        assert_eq!(diag.fatal(), None);
        assert_eq!(diag.last_error(), None);
    }

    #[test]
    fn snippet_truncates_on_a_char_boundary() {
        let long = "é".repeat(BODY_SNIPPET * 2);
        let s = snippet(&long);
        assert_eq!(s.chars().count(), BODY_SNIPPET + 1);
        assert!(s.ends_with('…'));
        assert_eq!(snippet("  short  "), "short");
    }
}
