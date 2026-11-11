use hyper::StatusCode;

use super::{Message, MessageType, WIRE_VERSION};

/// Noise protocol errors
#[derive(Debug, thiserror::Error)]
pub enum NoiseError {
    #[error("Noise protocol error: {0}")]
    Noise(#[from] tokio_noise::NoiseError),

    #[error("Hyper error: {0}")]
    Hyper(#[from] hyper::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] http::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    JsonSerialization(#[from] serde_json::Error),

    /// Non-2xx from the peer. The second field carries the peer's own reason
    /// when it sent one (a `MessageType::Error` frame on its deny path), which
    /// is the difference between "peer refused us" and "peer is down" — the
    /// two used to be indistinguishable at the requester (see #332).
    #[error(
        "Bad status code: {status}{reason}",
        status = .0,
        reason = .1.as_deref().map(|r| format!(" — peer said: {r}")).unwrap_or_default(),
    )]
    BadStatusCode(StatusCode, Option<String>),

    /// The peer refused our frame and answered with a frame on its own wire
    /// version, so the key is fine; one side must upgrade.
    #[error(
        "Noise wire version mismatch: peer speaks 0x{peer:02x}, this node speaks \
         0x{WIRE_VERSION:02x} — {side}",
        side = upgrade_side(*peer),
    )]
    WireVersionMismatch { peer: u8 },

    #[error("Invalid URI: {0}")]
    InvalidUri(#[from] http::uri::InvalidUri),

    #[error("URI parsing error: {0}")]
    UriParsingError(String),

    #[error("Request timeout")]
    RequestTimeout,

    #[error("TCP connection timeout: {0}")]
    TcpConnectionTimeout(String),

    #[error("TCP connection failed: {0}")]
    TcpConnectionFailed(String),

    #[error("Handshake failed")]
    HandshakeFailed,

    #[error("Unknown peer: {0}")]
    UnknownPeer(String),

    #[error("Decryption error")]
    DecryptionError,

    #[error("Invalid message format")]
    InvalidMessage,

    #[error("General error: {0}")]
    Anyhow(#[from] anyhow::Error),
}

impl From<hyper_noise::ClientError> for NoiseError {
    fn from(e: hyper_noise::ClientError) -> Self {
        match e {
            hyper_noise::ClientError::Hyper(hyper_err) => NoiseError::Hyper(hyper_err),
            hyper_noise::ClientError::Noise(noise_err) => NoiseError::Noise(noise_err),
            hyper_noise::ClientError::RequestTimeout => NoiseError::RequestTimeout,
        }
    }
}

impl NoiseError {
    /// Classify the body a peer sent with a non-2xx status.
    ///
    /// The listener answers a refusal with an `Error` frame that starts with
    /// its own `WIRE_VERSION`. A different byte there means the session keys
    /// matched and the builds do not. A byte up to 0x03 is a legacy frame's
    /// `MessageType` high byte, which carries no version to report.
    pub(crate) fn from_refusal(status: StatusCode, body: &[u8]) -> Self {
        match body.first().copied() {
            None | Some(0x00..=0x03) => Self::BadStatusCode(status, None),
            Some(WIRE_VERSION) => {
                let reason = Message::from_bytes(body)
                    .ok()
                    .filter(|m| m.msg_type == MessageType::Error)
                    .map(|m| String::from_utf8_lossy(&m.payload).into_owned());
                Self::BadStatusCode(status, reason)
            }
            Some(peer) => Self::WireVersionMismatch { peer },
        }
    }
}

/// Which side of a wire-version mismatch must upgrade. `WIRE_VERSION` only
/// increases, so the lower byte is the older build.
fn upgrade_side(peer: u8) -> &'static str {
    if peer < WIRE_VERSION {
        "the peer runs an older decman and must upgrade"
    } else {
        "this node runs an older decman and must upgrade"
    }
}

/// Returns `true` if a `NoiseError` represents a transient condition that
/// is worth retrying. Deterministic errors (handshake failures, 4xx status,
/// decode errors, configuration mistakes) are not retried; 5xx responses
/// are treated as transient (server-side hiccup).
///
/// Exhaustive match (no wildcard) — adding a new `NoiseError` variant will
/// fail to compile here until it's explicitly classified as retryable or not.
pub(crate) fn is_transient(err: &NoiseError) -> bool {
    match err {
        NoiseError::TcpConnectionTimeout(_)
        | NoiseError::TcpConnectionFailed(_)
        | NoiseError::RequestTimeout
        | NoiseError::Io(_)
        | NoiseError::Hyper(_) => true,
        NoiseError::BadStatusCode(code, _) => code.is_server_error(),
        NoiseError::Noise(_)
        | NoiseError::HandshakeFailed
        | NoiseError::WireVersionMismatch { .. }
        | NoiseError::DecryptionError
        | NoiseError::InvalidMessage
        | NoiseError::JsonSerialization(_)
        | NoiseError::Http(_)
        | NoiseError::InvalidUri(_)
        | NoiseError::UriParsingError(_)
        | NoiseError::UnknownPeer(_)
        | NoiseError::Anyhow(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::test_support::refusal_on;

    #[test]
    fn from_refusal_reports_an_older_peer_version() {
        let older = WIRE_VERSION - 1;
        let err = NoiseError::from_refusal(StatusCode::SERVICE_UNAVAILABLE, &refusal_on(older));

        assert!(
            matches!(err, NoiseError::WireVersionMismatch { peer } if peer == older),
            "expected a version mismatch, got: {err:?}"
        );
        let rendered = err.to_string();
        for needle in [
            format!("0x{older:02x}"),
            format!("0x{WIRE_VERSION:02x}"),
            "the peer runs an older".to_string(),
        ] {
            assert!(
                rendered.contains(&needle),
                "{needle:?} missing from: {rendered}"
            );
        }
    }

    #[test]
    fn from_refusal_reports_a_newer_peer_version() {
        let newer = WIRE_VERSION + 1;
        let err = NoiseError::from_refusal(StatusCode::SERVICE_UNAVAILABLE, &refusal_on(newer));

        assert!(
            matches!(err, NoiseError::WireVersionMismatch { peer } if peer == newer),
            "expected a version mismatch, got: {err:?}"
        );
        let rendered = err.to_string();
        for needle in [
            format!("0x{newer:02x}"),
            format!("0x{WIRE_VERSION:02x}"),
            "this node runs an older".to_string(),
        ] {
            assert!(
                rendered.contains(&needle),
                "{needle:?} missing from: {rendered}"
            );
        }
    }

    /// A legacy frame starts with its `MessageType` high byte, 0x00 to 0x03.
    /// None of those may read as a wire version.
    #[test]
    fn from_refusal_ignores_a_legacy_frame() {
        for msg_type in [
            MessageType::UploadDars,
            MessageType::Ack,
            MessageType::AddPartyKeysUpload,
            MessageType::ChunkedCommand,
        ] {
            let mut legacy = msg_type.to_u16().to_be_bytes().to_vec();
            legacy.extend_from_slice(&3u32.to_be_bytes());
            legacy.extend_from_slice(b"old");

            let err = NoiseError::from_refusal(StatusCode::SERVICE_UNAVAILABLE, &legacy);

            assert!(
                matches!(
                    err,
                    NoiseError::BadStatusCode(StatusCode::SERVICE_UNAVAILABLE, None)
                ),
                "{msg_type:?} frame read as: {err:?}"
            );
        }
    }

    #[test]
    fn from_refusal_ignores_an_empty_body() {
        let err = NoiseError::from_refusal(StatusCode::SERVICE_UNAVAILABLE, &[]);

        assert!(
            matches!(
                err,
                NoiseError::BadStatusCode(StatusCode::SERVICE_UNAVAILABLE, None)
            ),
            "got: {err:?}"
        );
    }

    #[test]
    fn from_refusal_keeps_the_reason_on_our_version() {
        let denied = Message::new(
            MessageType::Error,
            b"request rejected: unknown sender".to_vec(),
        )
        .to_bytes();
        let pong = Message::new_empty(MessageType::Pong).to_bytes();

        let denied = NoiseError::from_refusal(StatusCode::SERVICE_UNAVAILABLE, &denied);
        let pong = NoiseError::from_refusal(StatusCode::SERVICE_UNAVAILABLE, &pong);

        assert!(
            matches!(
                &denied,
                NoiseError::BadStatusCode(StatusCode::SERVICE_UNAVAILABLE, Some(reason))
                    if reason == "request rejected: unknown sender"
            ),
            "got: {denied:?}"
        );
        assert!(
            matches!(
                pong,
                NoiseError::BadStatusCode(StatusCode::SERVICE_UNAVAILABLE, None)
            ),
            "got: {pong:?}"
        );
    }

    #[test]
    fn wire_version_mismatch_is_not_transient() {
        assert!(!is_transient(&NoiseError::WireVersionMismatch {
            peer: WIRE_VERSION - 1,
        }));
    }
}
