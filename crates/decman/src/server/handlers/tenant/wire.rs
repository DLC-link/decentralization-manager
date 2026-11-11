//! Request decoding and error responses shared by the tenant flows.

use actix_web::HttpResponse;
use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{server::types::ErrorResponse, workflow::external_party::add_hosts::AddHostsError};

/// Base64-decode a raw Ed25519 public key into its fixed 32-byte array, or the
/// 400 response to return.
#[allow(clippy::result_large_err)]
pub(super) fn decode_public_key(encoded: &str) -> std::result::Result<[u8; 32], HttpResponse> {
    let bytes = STANDARD.decode(encoded).map_err(|e| {
        HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("public_key is not valid base64: {e}"),
        })
    })?;
    bytes.try_into().map_err(|b: Vec<u8>| {
        HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("public_key must be 32 bytes, got {len}", len = b.len()),
        })
    })
}

/// Base64-decode every entry of `encoded`, or the 400 response to return.
/// `what` names the field in the error.
/// Returns the message rather than a built response: an `HttpResponse` in the
/// `Err` arm is an order of magnitude larger than the `Ok` value, which clippy
/// rightly objects to for a helper called on every request.
pub(super) fn decode_all(
    encoded: &[String],
    what: &str,
) -> std::result::Result<Vec<Vec<u8>>, String> {
    encoded
        .iter()
        .map(|value| {
            STANDARD
                .decode(value)
                .map_err(|e| format!("{what} is not valid base64: {e}"))
        })
        .collect()
}

/// Map an [`AddHostsError`] onto the status code that tells the wallet what to
/// do about it: re-read and retry (409), fix the request (400/404), or treat
/// this host as unhealthy (500). Collapsing these into one code is the bug this
/// exists to prevent.
pub(super) fn add_hosts_error_response(stage: &str, error: AddHostsError) -> HttpResponse {
    match error {
        AddHostsError::UnknownParty { .. } => {
            tracing::info!("tenant add-hosts {stage}: {error}");
            HttpResponse::NotFound().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        AddHostsError::StaleSerial { .. } => {
            tracing::info!("tenant add-hosts {stage}: {error}");
            HttpResponse::Conflict().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        AddHostsError::Invalid(_) => {
            tracing::warn!("tenant add-hosts {stage}: refused the request: {error}");
            HttpResponse::BadRequest().json(ErrorResponse {
                error: error.to_string(),
            })
        }
        AddHostsError::Canton(_) => {
            // The chain goes to the log, not the body. A tonic transport error
            // inside it names the admin endpoint address, and this response
            // crosses a tenant API boundary to a wallet provider — same class of
            // leak as omnibus#46. Nothing in the chain is actionable to the
            // caller anyway: a Canton failure is ours to fix.
            tracing::error!("tenant add-hosts {stage}: Canton call failed: {error:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: "A Canton call failed on this host; see the host's logs".to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_all_accepts_well_formed_base64() {
        let encoded = vec![STANDARD.encode([1u8, 2, 3]), STANDARD.encode([4u8, 5])];
        let Ok(decoded) = decode_all(&encoded, "signature") else {
            panic!("well-formed base64 must decode");
        };
        assert_eq!(decoded, vec![vec![1u8, 2, 3], vec![4u8, 5]]);
    }

    /// One bad entry fails the whole batch rather than being silently dropped —
    /// a dropped entry would break the index alignment the signatures rely on.
    #[test]
    fn decode_all_rejects_the_whole_batch_on_one_bad_entry() {
        let encoded = vec![STANDARD.encode([1u8]), "not base64!".to_string()];
        assert!(
            decode_all(&encoded, "signature").is_err(),
            "a batch with an undecodable entry must be refused whole"
        );
    }

    #[test]
    fn decode_all_maps_an_empty_input_to_an_empty_batch() {
        let Ok(decoded) = decode_all(&[], "signature") else {
            panic!("an empty batch must decode");
        };
        assert!(decoded.is_empty());
    }

    /// The bug this guards: one status code for every failure sends the wallet
    /// to the wrong remedy. A stale pin means "re-read and retry"; a refused
    /// bundle means "fix your request"; a Canton failure means "this host is
    /// unhealthy". They must not collapse.
    #[test]
    fn add_hosts_errors_map_to_distinct_status_codes() {
        let cases = [
            (
                AddHostsError::UnknownParty {
                    party: "alice::1220aa".to_string(),
                },
                actix_web::http::StatusCode::NOT_FOUND,
            ),
            (
                AddHostsError::StaleSerial {
                    party: "alice::1220aa".to_string(),
                    pinned: 4,
                    found: 5,
                },
                actix_web::http::StatusCode::CONFLICT,
            ),
            (
                AddHostsError::Invalid(anyhow::anyhow!("drops current host")),
                actix_web::http::StatusCode::BAD_REQUEST,
            ),
            (
                AddHostsError::Canton(anyhow::anyhow!("AddTransactions RPC failed")),
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, expected) in cases {
            let label = error.to_string();
            assert_eq!(
                add_hosts_error_response("prepare", error).status(),
                expected,
                "wrong status for {label}"
            );
        }
    }

    /// A stale-pin error must name both serials, or the wallet cannot tell what
    /// to re-read to.
    #[test]
    fn stale_serial_names_both_serials() {
        let message = AddHostsError::StaleSerial {
            party: "alice::1220aa".to_string(),
            pinned: 4,
            found: 7,
        }
        .to_string();
        assert!(message.contains('4'), "{message}");
        assert!(message.contains('7'), "{message}");
    }
}
