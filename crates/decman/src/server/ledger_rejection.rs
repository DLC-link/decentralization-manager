//! What a rejected Ledger API submission says about its cause.
//!
//! Canton answers a failed submission with a gRPC status. The status message
//! starts with the Canton error id. The `grpc-status-details-bin` trailer holds
//! a `google.rpc.Status`, and its `ErrorInfo` detail carries the error's
//! context fields as metadata.

use actix_web::http::StatusCode;
use canton_proto_rs::google::rpc::{ErrorInfo, Status as RpcStatus};
use prost::Message;
use tonic::Code;

/// The id of a rejection for confirmations the mediator never received.
const MEDIATOR_TIMEOUT_ID: &str = "MEDIATOR_SAYS_TX_TIMED_OUT";

/// The `ErrorInfo` metadata key that lists the parties that did not confirm.
const UNRESPONSIVE_PARTIES_KEY: &str = "unresponsiveParties";

/// The id of a rejection for a command that Canton stopped tracking before it
/// knew the outcome.
const REQUEST_TIMEOUT_ID: &str = "REQUEST_TIME_OUT";

/// The ids of a refusal that a Daml contract raised itself. The message after
/// the id is the contract's own, which the operator acts on.
const CONTRACT_REFUSAL_IDS: [&str; 3] = [
    "UNHANDLED_EXCEPTION",
    "DAML_FAILURE",
    "INTERPRETATION_USER_ERROR",
];

/// The most bytes of a contract's refusal message that a failure keeps.
const MAX_CONTRACT_MESSAGE_LEN: usize = 512;

/// A governance write could not open its Ledger API channel. The cause can
/// name a certificate file or an address, so the answer gives only this text.
/// The server log keeps the cause.
#[derive(Debug, thiserror::Error)]
#[error("the Ledger API connection failed")]
pub(crate) struct LedgerConnectionFailed;

/// The Canton error id a failed submission carries, i.e. the message prefix
/// before `(`. `None` when the error is not a ledger rejection at all.
pub(crate) fn canton_error_id(e: &anyhow::Error) -> Option<String> {
    let status = e.downcast_ref::<tonic::Status>()?;
    let id = status.message().split('(').next()?.trim();
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
        .then(|| id.to_string())
}

/// A rejection by the mediator because some confirming parties did not
/// answer within the confirmation response timeout.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MediatorTimeout {
    /// The parties that did not answer. Empty when the rejection carried no
    /// readable details.
    pub unresponsive: Vec<String>,
}

impl MediatorTimeout {
    /// The cause in words an operator can act on.
    fn explanation(&self) -> String {
        let cause =
            format!("the mediator timed out waiting for confirmations ({MEDIATOR_TIMEOUT_ID}).");
        if self.unresponsive.is_empty() {
            return format!("{cause} Canton did not name the unresponsive party.");
        }
        format!(
            "{cause} Unresponsive: {}. The participant that hosts each of these parties must \
             be online to confirm.",
            self.unresponsive.join(", ")
        )
    }
}

/// The mediator timeout behind a failed submission, or `None` when the
/// failure is anything else.
pub(crate) fn mediator_timeout(e: &anyhow::Error) -> Option<MediatorTimeout> {
    if canton_error_id(e)? != MEDIATOR_TIMEOUT_ID {
        return None;
    }
    let status = e.downcast_ref::<tonic::Status>()?;
    Some(MediatorTimeout {
        unresponsive: unresponsive_parties(status.details()),
    })
}

/// The `unresponsiveParties` list of the `ErrorInfo` in `details`. Canton
/// writes it as one comma-separated string.
fn unresponsive_parties(details: &[u8]) -> Vec<String> {
    let Ok(status) = RpcStatus::decode(details) else {
        return Vec::new();
    };
    status
        .details
        .iter()
        .filter(|any| any.type_url.rsplit('/').next() == Some("google.rpc.ErrorInfo"))
        .filter_map(|any| ErrorInfo::decode(any.value.as_slice()).ok())
        .find_map(|info| info.metadata.get(UNRESPONSIVE_PARTIES_KEY).cloned())
        .map(|parties| {
            parties
                .split(',')
                .map(str::trim)
                .filter(|party| !party.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Why the outcome of a failed submission is unknown, or `None` when the
/// failure is definite. DecMan's own deadline reaches the caller as a
/// `CANCELLED` status from the gRPC client.
fn unknown_outcome(e: &anyhow::Error) -> Option<&'static str> {
    if canton_error_id(e).as_deref() == Some(REQUEST_TIMEOUT_ID) {
        return Some(REQUEST_TIMEOUT_ID);
    }
    match e.downcast_ref::<tonic::Status>()?.code() {
        Code::DeadlineExceeded => Some("DEADLINE_EXCEEDED"),
        Code::Cancelled => Some("CANCELLED"),
        _ => None,
    }
}

/// A failure's cause in words that hold no internal detail: no address, no
/// file name, no transport text. Any signed-in user can read the audit trail
/// that stores it. A ledger answer gives its Canton id or gRPC code, and a
/// contract's own refusal also keeps its message, escaped. A failed
/// connection says so. Any other error gives its outermost message, which
/// DecMan writes, unless that is a bare HTTP client error.
fn failure_summary(e: &anyhow::Error) -> String {
    if let Some(status) = e.downcast_ref::<tonic::Status>() {
        return match canton_error_id(e) {
            Some(id) if CONTRACT_REFUSAL_IDS.contains(&id.as_str()) => {
                let message = status.message().split_once("): ").map_or("", |(_, m)| m);
                let message = printable(message, MAX_CONTRACT_MESSAGE_LEN);
                let message = message.trim_end_matches('.');
                if message.is_empty() {
                    format!("the ledger refused the submission ({id})")
                } else {
                    format!("the ledger refused the submission ({id}): {message}")
                }
            }
            Some(id) => format!("the ledger refused the submission ({id})"),
            None => format!("the Ledger API answered {:?}", status.code()),
        };
    }
    if e.downcast_ref::<tonic::transport::Error>().is_some()
        || e.downcast_ref::<LedgerConnectionFailed>().is_some()
    {
        return LedgerConnectionFailed.to_string();
    }
    let outermost: &(dyn std::error::Error + Send + Sync + 'static) = e.as_ref();
    if outermost.is::<reqwest::Error>() {
        return "a request to another service failed".to_string();
    }
    e.to_string().trim_end_matches('.').to_string()
}

/// `text` with printable ASCII kept as it is and every other character
/// escaped, cut at `max` bytes. A contract's message then cannot break a log
/// line or reverse the text around it.
fn printable(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let piece: String = if c == ' ' || (c.is_ascii_graphic() && c != '\\') {
            c.to_string()
        } else {
            c.escape_default().collect()
        };
        if out.len() + piece.len() > max {
            out.push('…');
            break;
        }
        out.push_str(&piece);
    }
    out
}

/// The status and message for a failed governance submission. `context` says
/// what failed, such as "Failed to expire confirmation".
///
/// A mediator timeout is a 504 that names the unresponsive parties: the
/// mediator rejected the write. A submission that stopped waiting before the
/// ledger answered is also a 504, but its write may still complete, so the
/// message says so. Any other failure is a 500 that names its cause without
/// internal detail. Every failure logs its whole error chain, because the
/// answer leaves the detail out. For a timeout, that chain holds Canton's
/// correlation id.
pub(crate) fn submission_failure(context: &str, e: &anyhow::Error) -> (StatusCode, String) {
    tracing::warn!("{context}: {e:#}");
    if let Some(timeout) = mediator_timeout(e) {
        return (
            StatusCode::GATEWAY_TIMEOUT,
            format!("{context}: {}", timeout.explanation()),
        );
    }
    if let Some(cause) = unknown_outcome(e) {
        return (
            StatusCode::GATEWAY_TIMEOUT,
            format!(
                "{context}: the outcome is unknown. DecMan stopped waiting before the ledger \
                 answered ({cause}), and the write may still complete. Run `governance pending` \
                 before you retry."
            ),
        );
    }
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!(
            "{context}: {}. See the DecMan server log.",
            failure_summary(e)
        ),
    )
}

/// Rejections in the shape Canton sends them.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::HashMap;

    use canton_proto_rs::google::rpc::{ErrorInfo, RequestInfo, RetryInfo, Status as RpcStatus};
    use prost::Message;

    /// The message Canton writes for a mediator timeout: the error id, the
    /// error category, and a correlation id, then the cause.
    const MEDIATOR_TIMEOUT_MESSAGE: &str = "MEDIATOR_SAYS_TX_TIMED_OUT(2,5e3c1a7b): Rejected \
        transaction as the mediator did not receive sufficient confirmations within the \
        expected timeframe.";

    fn any(type_name: &str, message: &impl Message) -> prost_types::Any {
        prost_types::Any {
            type_url: format!("type.googleapis.com/{type_name}"),
            value: message.encode_to_vec(),
        }
    }

    /// Canton's rejection for a command it stopped tracking before it knew
    /// the outcome.
    pub(crate) fn request_timeout() -> tonic::Status {
        tonic::Status::deadline_exceeded(
            "REQUEST_TIME_OUT(3,5e3c1a7b): The request was not processed in time; its \
             outcome is unknown.",
        )
    }

    /// A mediator timeout whose `ErrorInfo` lists `unresponsive` as Canton
    /// does: one comma-separated string. Canton packs `ErrorInfo` first, then
    /// `RetryInfo` for a retryable category, then `RequestInfo`.
    pub(crate) fn mediator_timeout(unresponsive: &str) -> tonic::Status {
        let error_info = ErrorInfo {
            reason: "MEDIATOR_SAYS_TX_TIMED_OUT".to_string(),
            domain: String::new(),
            metadata: HashMap::from([
                ("category".to_string(), "2".to_string()),
                ("unresponsiveParties".to_string(), unresponsive.to_string()),
                ("tid".to_string(), "5e3c1a7b9d2f4e60".to_string()),
            ]),
        };
        let retry_info = RetryInfo {
            retry_delay: Some(prost_types::Duration {
                seconds: 1,
                nanos: 0,
            }),
        };
        let request_info = RequestInfo {
            request_id: "5e3c1a7b9d2f4e60".to_string(),
            serving_data: String::new(),
        };
        let details = RpcStatus {
            code: tonic::Code::Aborted as i32,
            message: MEDIATOR_TIMEOUT_MESSAGE.to_string(),
            details: vec![
                any("google.rpc.ErrorInfo", &error_info),
                any("google.rpc.RetryInfo", &retry_info),
                any("google.rpc.RequestInfo", &request_info),
            ],
        };
        tonic::Status::with_details(
            tonic::Code::Aborted,
            MEDIATOR_TIMEOUT_MESSAGE,
            details.encode_to_vec().into(),
        )
    }

    /// A mediator timeout without the details trailer.
    pub(crate) fn mediator_timeout_without_details() -> tonic::Status {
        tonic::Status::aborted(MEDIATOR_TIMEOUT_MESSAGE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: &str = "alice::1220aaaa";
    const BOB: &str = "bob::1220bbbb";

    #[test]
    fn a_mediator_timeout_names_its_unresponsive_parties() {
        let e = anyhow::Error::new(fixtures::mediator_timeout(&format!("{ALICE},{BOB}")));

        assert_eq!(
            mediator_timeout(&e),
            Some(MediatorTimeout {
                unresponsive: vec![ALICE.to_string(), BOB.to_string()],
            })
        );
    }

    #[test]
    fn a_mediator_timeout_without_details_is_still_recognized() {
        let e = anyhow::Error::new(fixtures::mediator_timeout_without_details());

        assert_eq!(
            mediator_timeout(&e),
            Some(MediatorTimeout {
                unresponsive: vec![]
            })
        );
    }

    #[test]
    fn a_mediator_timeout_with_undecodable_details_is_still_recognized() {
        let status = fixtures::mediator_timeout_without_details();
        let e = anyhow::Error::new(tonic::Status::with_details(
            status.code(),
            status.message(),
            vec![0xff, 0xff, 0xff].into(),
        ));

        assert_eq!(
            mediator_timeout(&e),
            Some(MediatorTimeout {
                unresponsive: vec![]
            })
        );
    }

    #[test]
    fn a_mediator_timeout_survives_added_context() {
        let e =
            anyhow::Error::new(fixtures::mediator_timeout(ALICE)).context("submitting the expire");

        assert_eq!(
            mediator_timeout(&e).map(|t| t.unresponsive),
            Some(vec![ALICE.to_string()])
        );
    }

    #[test]
    fn another_rejection_is_not_a_mediator_timeout() {
        let e = anyhow::Error::new(tonic::Status::not_found(
            "CONTRACT_NOT_FOUND(11,5e3c1a7b): Contract could not be found with id 00c0ffee",
        ));

        assert_eq!(mediator_timeout(&e), None);
    }

    #[test]
    fn a_non_ledger_error_is_not_a_mediator_timeout() {
        let e = anyhow::anyhow!("MEDIATOR_SAYS_TX_TIMED_OUT(2,0): not from the ledger");

        assert_eq!(mediator_timeout(&e), None);
    }

    #[test]
    fn a_mediator_timeout_answers_504_with_the_parties() {
        let e = anyhow::Error::new(fixtures::mediator_timeout(&format!("{ALICE},{BOB}")));

        let (status, message) = submission_failure("Failed to expire confirmation", &e);

        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            message,
            format!(
                "Failed to expire confirmation: the mediator timed out waiting for \
                 confirmations (MEDIATOR_SAYS_TX_TIMED_OUT). Unresponsive: {ALICE}, {BOB}. \
                 The participant that hosts each of these parties must be online to confirm."
            )
        );
    }

    #[test]
    fn a_mediator_timeout_without_parties_says_so() {
        let e = anyhow::Error::new(fixtures::mediator_timeout_without_details());

        let (status, message) = submission_failure("Failed to cancel confirmation", &e);

        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert!(
            message.ends_with("Canton did not name the unresponsive party."),
            "{message}"
        );
    }

    /// Canton names the id in the message whatever the gRPC code, so the id
    /// alone marks the outcome as unknown.
    #[test]
    fn a_request_time_out_has_an_unknown_outcome_under_any_code() {
        let e = anyhow::Error::new(tonic::Status::unavailable(
            "REQUEST_TIME_OUT(3,5e3c1a7b): The request was not processed in time.",
        ));

        let (status, message) = submission_failure("Failed to cancel proposal", &e);

        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert!(message.contains("(REQUEST_TIME_OUT)"), "{message}");
    }

    /// A mediator timeout is a rejection: the write did not land.
    #[test]
    fn a_mediator_timeout_is_not_an_unknown_outcome() {
        let e = anyhow::Error::new(fixtures::mediator_timeout(ALICE));

        let (_, message) = submission_failure("Failed to expire confirmation", &e);

        assert!(!message.contains("outcome is unknown"), "{message}");
    }

    #[test]
    fn another_failure_keeps_its_500() {
        let e = anyhow::anyhow!("connection refused").context("Failed to connect to ledger");

        let (status, message) = submission_failure("Failed to expire confirmation", &e);

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            message,
            "Failed to expire confirmation: Failed to connect to ledger. See the DecMan server \
             log."
        );
    }

    /// A ledger answer carries its message and metadata. Only the Canton id
    /// reaches the caller and the audit trail.
    #[test]
    fn a_ledger_refusal_gives_only_its_id() {
        let mut status = tonic::Status::not_found(
            "CONTRACT_NOT_FOUND(11,5e3c1a7b): Contract could not be found with id 00c0ffee",
        );
        status
            .metadata_mut()
            .insert("x-upstream", "ledger.internal:5001".parse().expect("ascii"));
        let e = anyhow::Error::new(status);

        let (status, message) = submission_failure("Failed to expire confirmation", &e);

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            message,
            "Failed to expire confirmation: the ledger refused the submission \
             (CONTRACT_NOT_FOUND). See the DecMan server log."
        );
    }

    /// A contract's own refusal message is what the operator acts on. It
    /// holds no internal detail, so it passes. A failed connection still
    /// names only its category, although its error names the address.
    #[tokio::test]
    async fn a_contract_refusal_keeps_its_message_and_a_connection_still_hides_its_address()
    -> anyhow::Result<()> {
        let refusal = anyhow::Error::new(tonic::Status::failed_precondition(
            "DAML_FAILURE(9,5e3c1a7b): the proposal needs one more confirmation.",
        ));

        let (status, message) = submission_failure("Failed to execute action", &refusal);

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            message,
            "Failed to execute action: the ledger refused the submission (DAML_FAILURE): the \
             proposal needs one more confirmation. See the DecMan server log."
        );

        let url = "http://127.0.0.1:1";
        let error = tonic::transport::Endpoint::from_static(url)
            .connect()
            .await
            .expect_err("nothing listens on port 1");
        let connection = anyhow::Error::new(error).context(format!("connecting to {url}"));

        let (_, message) = submission_failure("Failed to execute action", &connection);

        assert_eq!(
            message,
            "Failed to execute action: the Ledger API connection failed. See the DecMan server \
             log."
        );
        Ok(())
    }

    /// A contract writes its message, so the message is escaped and cut. It
    /// cannot break the log line or the web UI text around it.
    #[test]
    fn a_contract_message_is_escaped_and_cut() {
        let text = format!("line one\nline two \u{202e}{}", "x".repeat(2000));
        for id in CONTRACT_REFUSAL_IDS {
            let e = anyhow::Error::new(tonic::Status::failed_precondition(format!(
                "{id}(9,5e3c1a7b): {text}"
            )));

            let (_, message) = submission_failure("Failed to execute action", &e);

            let cause =
                format!("Failed to execute action: the ledger refused the submission ({id}): ");
            let kept = message
                .strip_prefix(&cause)
                .and_then(|rest| rest.strip_suffix("…. See the DecMan server log."))
                .unwrap_or_else(|| panic!("{message}"));
            assert!(
                kept.starts_with("line one\\nline two \\u{202e}xxx"),
                "{message}"
            );
            assert!(kept.len() <= MAX_CONTRACT_MESSAGE_LEN, "{message}");
            assert!(kept.len() > MAX_CONTRACT_MESSAGE_LEN - 8, "{message}");
        }
    }

    /// Another Canton refusal keeps only its id, as its message may name a
    /// contract or a participant.
    #[test]
    fn another_refusal_still_gives_only_its_id() {
        let e = anyhow::Error::new(tonic::Status::failed_precondition(
            "CONTRACT_NOT_ACTIVE(9,5e3c1a7b): Interpretation error: contract 00c0ffee is not \
             active.",
        ));

        let (_, message) = submission_failure("Failed to execute action", &e);

        assert_eq!(
            message,
            "Failed to execute action: the ledger refused the submission \
             (CONTRACT_NOT_ACTIVE). See the DecMan server log."
        );
    }

    /// An HTTP client error names the address it failed to reach.
    #[tokio::test]
    async fn a_bare_http_failure_hides_its_address() {
        let error = reqwest::Client::new()
            .get("http://127.0.0.1:1/internal")
            .send()
            .await
            .expect_err("nothing listens on port 1");
        let e = anyhow::Error::new(error);

        let (_, message) = submission_failure("Failed to execute action", &e);

        assert_eq!(
            message,
            "Failed to execute action: a request to another service failed. See the DecMan \
             server log."
        );
    }
}
