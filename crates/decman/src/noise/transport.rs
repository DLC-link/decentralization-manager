use std::time::Duration;

use bytes::Bytes;
use http::Uri;
use hyper::{Body, Request, StatusCode};
use tokio::net::TcpStream;
use tokio_noise::handshakes::nn_psk2::Initiator;

use crate::{config::NoiseRetryConfig, error::Result};

use super::{
    CHUNK_SIZE, MAX_CHUNK_COUNT, MAX_CHUNKED_TOTAL_SIZE, Message, MessageType, NoiseError,
    is_transient,
};

/// Timeout for control-plane Noise requests (command polls, acks, small
/// messages): covers connect + handshake + one round-trip. Kept short so a
/// dead coordinator is detected quickly.
pub const NOISE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-phase timeout for fetching a single chunk (connect, request, and the
/// response-body read are each bounded by this). A healthy 1 MiB chunk
/// transfers in well under a second; this is sized to absorb load spikes while
/// still catching a *stalled* chunk quickly so the caller can retry it on a
/// fresh connection (see `request_chunk_with_retry`) rather than waiting on the
/// server's handler timeout.
pub const NOISE_CHUNK_TIMEOUT: Duration = Duration::from_secs(25);

/// Server-side per-connection handler timeout (`hyper-noise` counts the full
/// response write against it). Acts as a backstop — the client now bounds its
/// own chunk reads via `NOISE_CHUNK_TIMEOUT`, so the client gives up (and
/// retries) first; this just bounds a connection the client already abandoned.
/// Kept comfortably above `NOISE_CHUNK_TIMEOUT`.
pub const NOISE_HANDLER_TIMEOUT: Duration = Duration::from_secs(45);

/// Per-request timeout for one ACS block fetch.
///
/// `NOISE_CHUNK_TIMEOUT` (25s) was sized for a 1 MiB chunk that "transfers in
/// well under a second" and is far too tight for an ACS block: at 25s a 4 MiB
/// block needs 1.4 Mbit/s sustained just to arrive, and `RequestTimeout` is
/// transient, so a link below that burns three attempts and a step strike on
/// every block without ever making progress.
///
/// 40s sits under the server's `NOISE_HANDLER_TIMEOUT` backstop, so the client
/// still gives up first and retries on a fresh connection, and it is the budget
/// [`acs_block_size`](super::acs_block_size) is clamped against.
pub const NOISE_ACS_BLOCK_TIMEOUT: Duration = Duration::from_secs(40);

/// Helper function to parse flexible URIs (with or without scheme)
pub fn parse_flexible_uri(uri_str: &str) -> Result<Uri, http::uri::InvalidUri> {
    let url = match uri_str.find("://") {
        None => format!("http://{uri_str}"),
        Some(_) => uri_str.to_string(),
    };

    url.parse::<Uri>()
}

/// Send a message to a peer using Noise protocol.
///
/// Public entry point preserved for backward compatibility — applies the
/// default `NOISE_REQUEST_TIMEOUT` to both the TCP connect and the
/// Noise/HTTP request budget. New callers wanting per-attempt control should
/// use `send_noise_message_with_retry`.
pub async fn send_noise_message(
    peer_address: &str,
    peer_port: u16,
    psk: &[u8; 32],
    identity: &[u8],
    message: &Message,
) -> Result<Bytes, NoiseError> {
    send_noise_message_with_timeout(
        peer_address,
        peer_port,
        psk,
        identity,
        message,
        NOISE_REQUEST_TIMEOUT,
    )
    .await
}

/// Inner implementation of `send_noise_message`, with per-step timeout
/// threaded through. Used both by the public single-shot entry point above
/// and by the retry wrapper.
async fn send_noise_message_with_timeout(
    peer_address: &str,
    peer_port: u16,
    psk: &[u8; 32],
    identity: &[u8],
    message: &Message,
    timeout: Duration,
) -> Result<Bytes, NoiseError> {
    let socket_addr = format!("{peer_address}:{peer_port}");

    let uri = parse_flexible_uri(&format!("http://{socket_addr}/message"))?;
    let request_body = message.to_bytes();

    // Each Noise call opens a fresh TCP connection and never reuses it, so ask
    // the server to close after replying. That drives hyper through
    // `poll_shutdown`, which blocking-drains tokio-noise's partial-write buffer;
    // the keep-alive path leaves the final packet's ciphertext stranded and the
    // client truncates a multi-packet response ("end of file before message
    // length reached"). See #242.
    let request = Request::builder()
        .uri(uri)
        .method("POST")
        .header(hyper::header::CONNECTION, "close")
        .body(Body::from(request_body))?;

    let tcp_stream = match tokio::time::timeout(timeout, TcpStream::connect(&socket_addr)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            return Err(NoiseError::TcpConnectionFailed(format!(
                "Failed to connect to {socket_addr}: {e}"
            )));
        }
        Err(_) => return Err(NoiseError::TcpConnectionTimeout(socket_addr.to_string())),
    };

    let initiator = Initiator { psk, identity };

    let mut response =
        hyper_noise::client::send_request(tcp_stream, initiator, request, Some(timeout)).await?;

    if response.status() != StatusCode::OK {
        // The refusal body names the peer's reason or its wire version.
        // Best-effort and bounded by the same timeout as the request.
        let status = response.status();
        let body =
            match tokio::time::timeout(timeout, hyper::body::to_bytes(response.body_mut())).await {
                Ok(Ok(bytes)) => bytes,
                _ => Bytes::new(),
            };
        return Err(NoiseError::from_refusal(status, &body));
    }

    let resp_body_bytes = hyper::body::to_bytes(response.body_mut()).await?;
    Ok(resp_body_bytes)
}

/// Run `op` up to `config.max_attempts` times, retrying only when the returned
/// `NoiseError` is classified as transient by `is_transient`. Sleeps
/// `config.backoff()` between attempts. Every failure is logged at `warn`,
/// including the final one: each caller handles the returned error, so the
/// caller decides whether it is worth an `error`. `peer_label` is used as a
/// structured field in the log lines.
async fn retry_loop<F, Fut>(
    peer_label: &str,
    config: &NoiseRetryConfig,
    mut op: F,
) -> Result<Bytes, NoiseError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Bytes, NoiseError>>,
{
    // Defense-in-depth: the CLI rejects 0 at parse time, but a misbehaving
    // direct construction of `NoiseRetryConfig` (e.g. in tests) shouldn't
    // panic in release.
    if config.max_attempts == 0 {
        return Err(NoiseError::Anyhow(anyhow::anyhow!(
            "NoiseRetryConfig.max_attempts must be >= 1"
        )));
    }
    let mut last_err: Option<NoiseError> = None;
    for attempt in 1..=config.max_attempts {
        match op().await {
            Ok(bytes) => return Ok(bytes),
            Err(e) if is_transient(&e) => {
                let will_retry = attempt < config.max_attempts;
                if will_retry {
                    tracing::warn!(
                        peer = peer_label,
                        attempt,
                        error = %e,
                        "noise: transient failure, retrying",
                    );
                } else {
                    tracing::warn!(
                        peer = peer_label,
                        attempt,
                        error = %e,
                        "noise: transient failure on final attempt",
                    );
                }
                last_err = Some(e);
                if will_retry {
                    tokio::time::sleep(config.backoff()).await;
                }
            }
            Err(e) => {
                tracing::warn!(
                    peer = peer_label,
                    attempt,
                    error = %e,
                    "noise: non-retryable failure",
                );
                return Err(e);
            }
        }
    }
    // Loop ran `max_attempts >= 1` iterations; the only way out without
    // returning is the transient branch, which sets `last_err`.
    let final_err = last_err.expect("retry_loop ran zero attempts");
    tracing::warn!(
        peer = peer_label,
        attempts = config.max_attempts,
        error = %final_err,
        "noise: peer unreachable after retry exhaustion",
    );
    Err(final_err)
}

/// Send a message to a peer with bounded retry on transient failures.
///
/// Up to `config.max_attempts` attempts, each governed by
/// `config.per_attempt_timeout()`, with `config.backoff()` between attempts.
/// Discriminating retry: only transient `NoiseError` variants (TCP connect
/// timeouts, refused connections, request timeouts, IO/Hyper failures) are
/// retried. Deterministic errors (handshake failure, bad status, decode
/// errors, configuration mistakes) return immediately.
///
/// Every failure logs at `tracing::warn!`, and retry exhaustion logs one more
/// `warn` line. The caller logs the returned error at the level it deserves.
pub async fn send_noise_message_with_retry(
    peer_address: &str,
    peer_port: u16,
    psk: &[u8; 32],
    identity: &[u8],
    message: &Message,
    config: &NoiseRetryConfig,
) -> Result<Bytes, NoiseError> {
    let peer_label = format!("{peer_address}:{peer_port}");
    let timeout = config.per_attempt_timeout();
    // `move || async move` — `&T` references are `Copy`, so each call to the
    // FnMut closure freshly copies the references into a new async block.
    // Without `move`, the borrow checker has trouble proving the returned
    // future doesn't outlive the closure's borrow.
    retry_loop(&peer_label, config, move || async move {
        send_noise_message_with_timeout(peer_address, peer_port, psk, identity, message, timeout)
            .await
    })
    .await
}

/// Parse the 10-byte metadata payload from a `MessageType::ChunkedCommand`
/// response: `[command_type:u16][total_size:u32][chunk_count:u32]`, all
/// big-endian.
///
/// Returns `(command_type, total_size, chunk_count)` on success.
fn parse_chunked_command_metadata(
    payload: &[u8],
) -> Result<(MessageType, usize, usize), NoiseError> {
    if payload.len() < 10 {
        return Err(NoiseError::InvalidMessage);
    }
    let command_type_u16 = u16::from_be_bytes([payload[0], payload[1]]);
    let total_size = u32::from_be_bytes([payload[2], payload[3], payload[4], payload[5]]) as usize;
    let chunk_count = u32::from_be_bytes([payload[6], payload[7], payload[8], payload[9]]) as usize;
    let command_type =
        MessageType::try_from(command_type_u16).map_err(|_| NoiseError::InvalidMessage)?;
    Ok((command_type, total_size, chunk_count))
}

/// Send a message that may receive a chunked response.
///
/// First call goes through `send_noise_message_with_retry`. If the response
/// is `MessageType::ChunkedCommand`, this function transparently fetches the
/// referenced chunks (one Noise call per chunk, each with retry) and
/// returns the **assembled** Message bytes — i.e. the final `Bytes` is the
/// `Message::to_bytes()` form of `Message::new(original_command_type,
/// reassembled_payload)`. Callers can decode it the same way they would a
/// non-chunked response.
///
/// If the response isn't chunked, the original bytes are returned unchanged.
///
/// Each chunk fetch is a fresh Noise connection (TCP connect + handshake +
/// 1 round-trip). Retry policy applies to each individual chunk fetch.
pub async fn send_noise_message_with_chunked_response(
    peer_address: &str,
    peer_port: u16,
    psk: &[u8; 32],
    identity: &[u8],
    message: &Message,
    config: &NoiseRetryConfig,
) -> Result<Bytes, NoiseError> {
    let response =
        send_noise_message_with_retry(peer_address, peer_port, psk, identity, message, config)
            .await?;

    let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

    if resp_msg.msg_type != MessageType::ChunkedCommand {
        // Not chunked — caller can decode `response` directly.
        return Ok(response);
    }

    let (command_type, total_size, chunk_count) =
        parse_chunked_command_metadata(&resp_msg.payload)?;

    // Bound peer-supplied metadata before we allocate or loop. A malicious or
    // buggy peer could otherwise advertise multi-GB sizes and trigger an OOM.
    if total_size > MAX_CHUNKED_TOTAL_SIZE || chunk_count > MAX_CHUNK_COUNT {
        tracing::warn!(
            peer = format!("{peer_address}:{peer_port}"),
            total_size,
            chunk_count,
            "noise: chunked-response metadata exceeds configured caps",
        );
        return Err(NoiseError::InvalidMessage);
    }
    // chunk_count must agree with total_size and CHUNK_SIZE; reject mismatch
    // (e.g. total=10 but chunk_count=1000).
    let expected_chunks = total_size.div_ceil(CHUNK_SIZE);
    if chunk_count != expected_chunks {
        tracing::warn!(
            peer = format!("{peer_address}:{peer_port}"),
            total_size,
            chunk_count,
            expected_chunks,
            "noise: chunked-response chunk_count inconsistent with total_size",
        );
        return Err(NoiseError::InvalidMessage);
    }

    tracing::debug!(
        peer = format!("{peer_address}:{peer_port}"),
        total_size,
        chunk_count,
        command = ?command_type,
        "noise: receiving chunked response"
    );

    let mut assembled = Vec::with_capacity(total_size);
    for chunk_index in 0..chunk_count {
        let chunk_request = Message::new(
            MessageType::GetChunk,
            (chunk_index as u32).to_be_bytes().to_vec(),
        );
        let chunk_response = send_noise_message_with_retry(
            peer_address,
            peer_port,
            psk,
            identity,
            &chunk_request,
            config,
        )
        .await?;
        let chunk_msg =
            Message::from_bytes(&chunk_response).map_err(|_| NoiseError::InvalidMessage)?;
        if chunk_msg.msg_type != MessageType::Chunk || chunk_msg.payload.len() < 4 {
            return Err(NoiseError::InvalidMessage);
        }
        // Chunk payload format: [chunk_index:4][chunk_data]
        // Verify the echoed index matches what we requested so a server bug
        // (or cache mix-up between concurrent peers) can't silently corrupt
        // the assembled payload.
        let received_index = u32::from_be_bytes([
            chunk_msg.payload[0],
            chunk_msg.payload[1],
            chunk_msg.payload[2],
            chunk_msg.payload[3],
        ]) as usize;
        if received_index != chunk_index {
            tracing::warn!(
                peer = format!("{peer_address}:{peer_port}"),
                requested = chunk_index,
                received = received_index,
                "noise: chunk response carried wrong chunk index",
            );
            return Err(NoiseError::InvalidMessage);
        }
        assembled.extend_from_slice(&chunk_msg.payload[4..]);
    }

    if assembled.len() != total_size {
        tracing::warn!(
            "noise: chunked-response assembly produced {} bytes but metadata declared {}",
            assembled.len(),
            total_size,
        );
        return Err(NoiseError::InvalidMessage);
    }

    // Re-encode as a complete Message of the original command type so the
    // caller can decode it exactly as if the response had arrived unchunked.
    let assembled_msg = Message::new(command_type, assembled);
    Ok(Bytes::from(assembled_msg.to_bytes()))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use bytes::Bytes;

    use super::*;
    use crate::{
        canton_id::CantonId,
        config::{NodeConfig, Peer},
        noise::{NoiseKeypair, WIRE_VERSION, client::NoiseClient, test_support::refusal_on},
    };

    fn test_retry_config() -> NoiseRetryConfig {
        // Zero backoff so the `retry_loop_*` tests don't sleep in real time.
        // (Tests that need to assert on backoff behavior should override.)
        NoiseRetryConfig {
            backoff_ms: 0,
            ..NoiseRetryConfig::default()
        }
    }

    #[test]
    fn test_parse_flexible_uri() -> Result {
        // With scheme
        let uri1 = parse_flexible_uri("http://example.com:8080")?;
        assert_eq!(uri1.host(), Some("example.com"));
        assert_eq!(uri1.port_u16(), Some(8080));

        // Without scheme
        let uri2 = parse_flexible_uri("example.com:8080")?;
        assert_eq!(uri2.host(), Some("example.com"));
        assert_eq!(uri2.port_u16(), Some(8080));
        Ok(())
    }

    #[tokio::test]
    async fn retry_loop_succeeds_on_first_attempt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let result = retry_loop("test-peer", &test_retry_config(), move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<Bytes, NoiseError>(Bytes::from_static(b"ok"))
            }
        })
        .await;
        assert!(matches!(result, Ok(b) if b.as_ref() == b"ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_loop_retries_on_transient_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let result = retry_loop("test-peer", &test_retry_config(), move || {
            let calls = calls_clone.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Err(NoiseError::TcpConnectionTimeout("test".into()))
                } else {
                    Ok(Bytes::from_static(b"ok"))
                }
            }
        })
        .await;
        assert!(matches!(result, Ok(b) if b.as_ref() == b"ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retry_loop_returns_terminal_error_after_two_transient_failures() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        // Pin max_attempts explicitly rather than relying on the default in
        // `test_retry_config()`, so the asserted call count (and this test's
        // "two failures" name) stay correct if the default ever changes.
        let config = NoiseRetryConfig {
            per_attempt_timeout_secs: 5,
            max_attempts: 2,
            backoff_ms: 0,
        };
        let result = retry_loop("test-peer", &config, move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, _>(NoiseError::TcpConnectionTimeout("test".into()))
            }
        })
        .await;
        assert!(matches!(result, Err(NoiseError::TcpConnectionTimeout(_))));
        assert_eq!(calls.load(Ordering::SeqCst), config.max_attempts);
    }

    /// Records the level of every event it sees.
    struct Levels(Arc<std::sync::Mutex<Vec<tracing::Level>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Levels {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if let Ok(mut levels) = self.0.lock() {
                levels.push(*event.metadata().level());
            }
        }
    }

    /// An unreachable peer is routine, and every caller handles the error on
    /// its own terms. An `error` here once made up every decman error line on
    /// devnet, so a decman-wide error alert would fire all day (#362).
    #[tokio::test]
    async fn retry_exhaustion_logs_no_error() -> anyhow::Result<()> {
        use tracing_subscriber::layer::SubscriberExt;

        let levels = Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Levels(levels.clone()));
        let _guard = tracing::subscriber::set_default(subscriber);
        let config = NoiseRetryConfig {
            per_attempt_timeout_secs: 5,
            max_attempts: 2,
            backoff_ms: 0,
        };

        let result = retry_loop("test-peer", &config, || async {
            Err::<Bytes, _>(NoiseError::TcpConnectionTimeout("test".into()))
        })
        .await;

        assert!(result.is_err());
        let levels = levels
            .lock()
            .map_err(|_| anyhow::anyhow!("the level log is poisoned"))?;
        assert!(!levels.is_empty(), "the failures logged nothing");
        assert!(
            !levels.contains(&tracing::Level::ERROR),
            "retry exhaustion logged an error: {levels:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn retry_loop_does_not_retry_on_4xx_bad_status() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let result = retry_loop("test-peer", &test_retry_config(), move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, _>(NoiseError::BadStatusCode(StatusCode::BAD_REQUEST, None))
            }
        })
        .await;
        assert!(matches!(result, Err(NoiseError::BadStatusCode(..))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// The peer answers 503, which alone would retry, but no retry can make
    /// two builds agree on a wire version.
    #[tokio::test]
    async fn retry_loop_does_not_retry_on_wire_version_mismatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let result = retry_loop("test-peer", &test_retry_config(), move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, _>(NoiseError::from_refusal(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &refusal_on(WIRE_VERSION - 1),
                ))
            }
        })
        .await;
        assert!(matches!(
            result,
            Err(NoiseError::WireVersionMismatch { .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_loop_retries_on_5xx_bad_status() {
        // 5xx is a server-side hiccup — treat as transient.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let result = retry_loop("test-peer", &test_retry_config(), move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, _>(NoiseError::BadStatusCode(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    None,
                ))
            }
        })
        .await;
        assert!(matches!(result, Err(NoiseError::BadStatusCode(..))));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retry_loop_does_not_retry_on_invalid_message() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let result = retry_loop("test-peer", &test_retry_config(), move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, _>(NoiseError::InvalidMessage)
            }
        })
        .await;
        assert!(matches!(result, Err(NoiseError::InvalidMessage)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_loop_does_not_retry_on_handshake_failed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let result = retry_loop("test-peer", &test_retry_config(), move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, _>(NoiseError::HandshakeFailed)
            }
        })
        .await;
        assert!(matches!(result, Err(NoiseError::HandshakeFailed)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_loop_returns_error_on_zero_max_attempts() {
        // Defense-in-depth: a `NoiseRetryConfig` constructed with max_attempts=0
        // must produce an error rather than panic in release builds.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let bad_config = NoiseRetryConfig {
            per_attempt_timeout_secs: 5,
            max_attempts: 0,
            backoff_ms: 0,
        };
        let result = retry_loop("test-peer", &bad_config, move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<Bytes, NoiseError>(Bytes::from_static(b"unreachable"))
            }
        })
        .await;
        assert!(matches!(result, Err(NoiseError::Anyhow(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn retry_loop_does_not_retry_on_anyhow() {
        // Anyhow is the catch-all for unknown wrapped errors; it must fail
        // closed (no retry) so a stray classification mistake doesn't turn it
        // into a retry-storm vector.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let result = retry_loop("test-peer", &test_retry_config(), move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, _>(NoiseError::Anyhow(anyhow::anyhow!("unknown")))
            }
        })
        .await;
        assert!(matches!(result, Err(NoiseError::Anyhow(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn parse_chunked_command_metadata_happy_path() {
        // Build a valid metadata payload: [Data:0x0102][total_size:1024][chunk_count:5]
        let mut payload = Vec::with_capacity(10);
        payload.extend_from_slice(&MessageType::Data.to_u16().to_be_bytes());
        payload.extend_from_slice(&1024u32.to_be_bytes());
        payload.extend_from_slice(&5u32.to_be_bytes());

        let (command_type, total_size, chunk_count) =
            parse_chunked_command_metadata(&payload).unwrap();
        assert_eq!(command_type, MessageType::Data);
        assert_eq!(total_size, 1024);
        assert_eq!(chunk_count, 5);
    }

    #[test]
    fn parse_chunked_command_metadata_too_short() {
        let payload = vec![0u8; 9]; // 1 byte short
        assert!(matches!(
            parse_chunked_command_metadata(&payload),
            Err(NoiseError::InvalidMessage)
        ));
    }

    #[test]
    fn parse_chunked_command_metadata_unknown_command_type() {
        // Set command_type bytes to 0xFFFF — not a valid MessageType variant.
        let mut payload = vec![0xFF, 0xFF];
        payload.extend_from_slice(&100u32.to_be_bytes());
        payload.extend_from_slice(&1u32.to_be_bytes());
        assert!(matches!(
            parse_chunked_command_metadata(&payload),
            Err(NoiseError::InvalidMessage)
        ));
    }

    /// Ciphertext bytes the link delivers at a time: five 2 KiB Noise frames.
    /// Their 10,150 plaintext bytes overrun hyper's initial 8 KiB read buffer
    /// partway into the fifth frame, so tokio-noise keeps the rest of that
    /// frame back, and the pause after the burst leaves the socket empty.
    const BURST: usize = 5 * 2048;

    /// Forward one connection to `server`, delivering the server's bytes in
    /// bursts with a pause between them, the way a real link does. Over plain
    /// loopback the body arrives faster than the client reads it, so the client
    /// never finds its socket empty mid-response.
    async fn paced_relay(
        mut client: TcpStream,
        server: std::net::SocketAddr,
    ) -> std::io::Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut server = TcpStream::connect(server).await?;
        let (mut client_read, mut client_write) = client.split();
        let (mut server_read, mut server_write) = server.split();
        let upstream = async {
            tokio::io::copy(&mut client_read, &mut server_write).await?;
            server_write.shutdown().await
        };
        let downstream = async {
            let mut burst = vec![0u8; BURST];
            loop {
                let n = server_read.read(&mut burst).await?;
                if n == 0 {
                    return client_write.shutdown().await;
                }
                client_write.write_all(&burst[..n]).await?;
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        };
        tokio::try_join!(upstream, downstream).map(|_| ())
    }

    /// Accept connections until aborted, handing each to `handle`.
    fn accept_loop<C, Fut>(
        listener: tokio::net::TcpListener,
        handle: C,
    ) -> tokio::task::JoinHandle<()>
    where
        C: Fn(TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(handle(socket));
            }
        })
    }

    /// A chunk-sized response arrives whole over a real Noise connection
    /// whose link delivers it in bursts (#441). tokio-noise used to return
    /// `Pending` after copying leftover plaintext into the caller's buffer,
    /// and hyper discarded those bytes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn chunk_sized_responses_arrive_whole() -> Result {
        const ROUNDS: usize = 16;
        let psk = [7u8; 32];
        // Position-dependent bytes, so a lost or repeated span cannot compare equal.
        let payload = (0..CHUNK_SIZE).map(|i| (i % 251) as u8).collect();
        let response = Bytes::from(Message::new(MessageType::Chunk, payload).to_bytes());

        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let server_addr = server.local_addr()?;
        let served = response.clone();
        let server = accept_loop(server, move |socket| {
            let body = served.clone();
            async move {
                let respond = move |_: &[u8], _: Request<Body>| {
                    let body = body.clone();
                    async move { Ok::<_, hyper::Error>(hyper::Response::new(Body::from(body))) }
                };
                let responder =
                    tokio_noise::handshakes::nn_psk2::Responder::new(move |_: &[u8]| Some(psk));
                if let Err(e) = hyper_noise::server::serve_http(
                    socket,
                    responder,
                    respond,
                    Some(NOISE_HANDLER_TIMEOUT),
                )
                .await
                {
                    tracing::warn!("test server connection failed: {e}");
                }
            }
        });
        let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = relay.local_addr()?.port();
        let relay = accept_loop(relay, move |socket| async move {
            if let Err(e) = paced_relay(socket, server_addr).await {
                tracing::warn!("test relay failed: {e}");
            }
        });

        let request = Message::new(MessageType::GetChunk, 0u32.to_be_bytes().to_vec());
        let mut failures = Vec::new();
        for round in 0..ROUNDS {
            match send_noise_message("127.0.0.1", port, &psk, b"peer", &request).await {
                Ok(bytes) if bytes == response => {}
                Ok(bytes) => failures.push(format!("round {round}: {} bytes", bytes.len())),
                Err(e) => failures.push(format!("round {round}: {e}")),
            }
        }
        server.abort();
        relay.abort();
        assert!(
            failures.is_empty(),
            "{} of {ROUNDS} rounds failed: {failures:?}",
            failures.len()
        );
        Ok(())
    }

    /// A peer listener that answers every request with `status` and `body`
    /// over a real Noise session. Returns its port and how many requests it
    /// served.
    async fn refusing_peer(
        psk: [u8; 32],
        status: StatusCode,
        body: Vec<u8>,
    ) -> Result<(u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let served = Arc::new(AtomicUsize::new(0));
        let counter = served.clone();
        let body = Bytes::from(body);
        let server = accept_loop(listener, move |socket| {
            let counter = counter.clone();
            let body = body.clone();
            async move {
                let respond = move |_: &[u8], _: Request<Body>| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let mut response = hyper::Response::new(Body::from(body.clone()));
                    *response.status_mut() = status;
                    async move { Ok::<_, hyper::Error>(response) }
                };
                let responder =
                    tokio_noise::handshakes::nn_psk2::Responder::new(move |_: &[u8]| Some(psk));
                if let Err(e) = hyper_noise::server::serve_http(
                    socket,
                    responder,
                    respond,
                    Some(NOISE_HANDLER_TIMEOUT),
                )
                .await
                {
                    tracing::warn!("test peer connection failed: {e}");
                }
            }
        });
        Ok((port, served, server))
    }

    fn three_attempts() -> NoiseRetryConfig {
        NoiseRetryConfig {
            per_attempt_timeout_secs: 5,
            max_attempts: 3,
            backoff_ms: 0,
        }
    }

    /// The probe path: an older peer refuses with a 503, and the client names
    /// the peer's byte without spending its retries on it.
    #[tokio::test]
    async fn an_older_peers_refusal_is_a_mismatch_after_one_attempt() -> Result {
        let psk = [9u8; 32];
        let older = WIRE_VERSION - 1;
        let (port, served, server) =
            refusing_peer(psk, StatusCode::SERVICE_UNAVAILABLE, refusal_on(older)).await?;

        let result = send_noise_message_with_retry(
            "127.0.0.1",
            port,
            &psk,
            b"self",
            &Message::new_empty(MessageType::Health),
            &three_attempts(),
        )
        .await;
        server.abort();

        assert!(
            matches!(result, Err(NoiseError::WireVersionMismatch { peer }) if peer == older),
            "got: {result:?}"
        );
        assert_eq!(served.load(Ordering::SeqCst), 1);
        Ok(())
    }

    /// Control for the test above: the same 503 with no body is retried, so
    /// the single attempt there comes from the body.
    #[tokio::test]
    async fn an_empty_503_is_retried_to_the_last_attempt() -> Result {
        let psk = [9u8; 32];
        let (port, served, server) =
            refusing_peer(psk, StatusCode::SERVICE_UNAVAILABLE, Vec::new()).await?;
        let config = three_attempts();

        let result = send_noise_message_with_retry(
            "127.0.0.1",
            port,
            &psk,
            b"self",
            &Message::new_empty(MessageType::Health),
            &config,
        )
        .await;
        server.abort();

        assert!(
            matches!(
                result,
                Err(NoiseError::BadStatusCode(
                    StatusCode::SERVICE_UNAVAILABLE,
                    None
                ))
            ),
            "got: {result:?}"
        );
        assert_eq!(served.load(Ordering::SeqCst), config.max_attempts);
        Ok(())
    }

    /// The single-shot fan-out path names a newer peer the same way.
    #[tokio::test]
    async fn a_newer_peers_refusal_is_a_mismatch_on_the_single_shot_path() -> Result {
        let psk = [9u8; 32];
        let newer = WIRE_VERSION + 1;
        let (port, served, server) =
            refusing_peer(psk, StatusCode::SERVICE_UNAVAILABLE, refusal_on(newer)).await?;

        let result = send_noise_message(
            "127.0.0.1",
            port,
            &psk,
            b"self",
            &Message::new_empty(MessageType::RequestOwnerKeys),
        )
        .await;
        server.abort();

        assert!(
            matches!(result, Err(NoiseError::WireVersionMismatch { peer }) if peer == newer),
            "got: {result:?}"
        );
        assert_eq!(served.load(Ordering::SeqCst), 1);
        Ok(())
    }

    /// A peer on our own version that refuses us still reports why (#332).
    #[tokio::test]
    async fn a_same_version_refusal_keeps_the_peers_reason() -> Result {
        let psk = [9u8; 32];
        let denied = Message::new(
            MessageType::Error,
            b"request rejected: unknown sender".to_vec(),
        );
        let (port, _, server) =
            refusing_peer(psk, StatusCode::SERVICE_UNAVAILABLE, denied.to_bytes()).await?;

        let result = send_noise_message(
            "127.0.0.1",
            port,
            &psk,
            b"self",
            &Message::new_empty(MessageType::Health),
        )
        .await;
        server.abort();

        assert!(
            matches!(
                &result,
                Err(NoiseError::BadStatusCode(StatusCode::SERVICE_UNAVAILABLE, Some(reason)))
                    if reason == "request rejected: unknown sender"
            ),
            "got: {result:?}"
        );
        Ok(())
    }

    /// The workflow client a peer uses to reach its coordinator names an
    /// older coordinator too.
    #[tokio::test]
    async fn the_workflow_client_names_an_older_coordinator() -> Result {
        let root = tempfile::tempdir()?;
        let mut config = NodeConfig::default().with_root_dir(root.path());
        config.node.participant_id =
            Some(CantonId::parse(&format!("peer::1220{}", "a".repeat(64)))?);
        let keys = NoiseKeypair::generate();
        tokio::fs::create_dir_all(config.data_dir()).await?;
        keys.save_to_file(config.key_file_path()).await?;
        let coordinator_keys = NoiseKeypair::generate();
        let psk = *coordinator_keys.derive_psk(&keys.public_key);
        let older = WIRE_VERSION - 1;
        let (port, served, server) =
            refusing_peer(psk, StatusCode::SERVICE_UNAVAILABLE, refusal_on(older)).await?;
        let coordinator = Peer {
            participant_id: CantonId::parse(&format!("coordinator::1220{}", "b".repeat(64)))?,
            name: "Older coordinator".into(),
            address: "127.0.0.1".into(),
            port,
            public_key: coordinator_keys.public_key_hex(),
            party: None,
        };
        let client = NoiseClient::new(config, coordinator, "run-1".into()).await?;

        let result = client
            .send_message(&Message::new_empty(MessageType::GetNextCommand))
            .await;
        server.abort();

        assert!(
            matches!(result, Err(NoiseError::WireVersionMismatch { peer }) if peer == older),
            "got: {result:?}"
        );
        assert_eq!(served.load(Ordering::SeqCst), 1);
        Ok(())
    }
}
