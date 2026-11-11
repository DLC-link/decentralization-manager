//! What a rejected Ledger API submission says about its cause.
//!
//! Canton answers a failed submission with a gRPC status. The status message
//! starts with the Canton error id. The `grpc-status-details-bin` trailer holds
//! a `google.rpc.Status`, and its `ErrorInfo` detail carries the error's
//! context fields as metadata.

use actix_web::http::StatusCode;
use canton_proto_rs::google::rpc::{ErrorInfo, Status as RpcStatus};
use prost::Message;

/// The id of a rejection for confirmations the mediator never received.
const MEDIATOR_TIMEOUT_ID: &str = "MEDIATOR_SAYS_TX_TIMED_OUT";

/// The `ErrorInfo` metadata key that lists the parties that did not confirm.
const UNRESPONSIVE_PARTIES_KEY: &str = "unresponsiveParties";

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

/// The status and message for a failed governance submission. `context` says
/// what failed, such as "Failed to expire confirmation".
///
/// A mediator timeout is a 504 that names the unresponsive parties. Any other
/// failure is a 500 that carries the whole error chain.
pub(crate) fn submission_failure(context: &str, e: &anyhow::Error) -> (StatusCode, String) {
    match mediator_timeout(e) {
        Some(timeout) => (
            StatusCode::GATEWAY_TIMEOUT,
            format!("{context}: {}", timeout.explanation()),
        ),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{context}: {e:#}"),
        ),
    }
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

    #[test]
    fn another_failure_keeps_its_500() {
        let e = anyhow::anyhow!("connection refused").context("Failed to connect to ledger");

        let (status, message) = submission_failure("Failed to expire confirmation", &e);

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            message,
            "Failed to expire confirmation: Failed to connect to ledger: connection refused"
        );
    }
}
