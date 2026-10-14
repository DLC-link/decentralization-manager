//! HTTP failure classification for polled `Then` probes.
//!
//! The [`penelope`] runner ends a polled step early on a repeated
//! [`Class::Fatal`] failure, and it adds the last failure to a timeout. Only
//! decman knows which of its HTTP failures can still clear, so
//! [`Failure::class`] decides that here and [`ProbeDiag`] carries the result
//! to the runner.
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
//! some background work completes would be failed here after [`FATAL_STREAK`]
//! polls — about six seconds — where before this module it would have polled
//! and eventually passed. Nothing enforces the requirement; it is on whoever
//! adds the next probe type.

use reqwest::StatusCode;

pub use penelope::{Class, FATAL_STREAK, ProbeDiag};

/// Longest failure body echoed into a log line or error message.
const BODY_SNIPPET: usize = 300;

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
    fn snippet_truncates_on_a_char_boundary() {
        let long = "é".repeat(BODY_SNIPPET * 2);
        let s = snippet(&long);
        assert_eq!(s.chars().count(), BODY_SNIPPET + 1);
        assert!(s.ends_with('…'));
        assert_eq!(snippet("  short  "), "short");
    }
}
