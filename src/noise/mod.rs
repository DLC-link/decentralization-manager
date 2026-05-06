pub mod client;
pub mod server;

use std::{marker::PhantomData, path::Path, time::Duration};

use anyhow::Context;
use bytes::Bytes;
use http::Uri;
use hyper::{Body, Request, StatusCode};
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_noise::handshakes::nn_psk2::Initiator;

use crate::{config::NoiseRetryConfig, error::Result};

/// Timeout for Noise protocol operations
pub const NOISE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Message types for the Noise protocol communication
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[repr(u16)]
pub enum MessageType {
    // Commands (0x0000 - 0x00FF)
    UploadDars = 0x0001,
    GenerateKeys = 0x0002,
    SignDns = 0x0003,
    SignP2p = 0x0004,
    SignSubmissions = 0x0005,
    StatusUpdate = 0x0006,
    Disconnect = 0x0007,
    GetNextCommand = 0x0008,
    SignKick = 0x0009,
    Ping = 0x000A,
    ListPackages = 0x000B,
    RequestOwnerKeys = 0x000C,
    ListPeers = 0x000D,
    RequestMemberParty = 0x000E,

    // Invites (0x0010 - 0x001F)
    InviteOnboarding = 0x0010,
    InviteKick = 0x0011,
    InviteContracts = 0x0012,
    InviteDars = 0x0013,

    // Responses (0x0100 - 0x01FF)
    Ack = 0x0101,
    Data = 0x0102,
    Error = 0x0103,
    Ready = 0x0104,
    Wait = 0x0105,
    Pong = 0x0106,
    OwnerKeys = 0x0107,
    PeerList = 0x0108,
    MemberPartyResponse = 0x0109,

    // Data Transfers (0x0200 - 0x02FF)
    KeysUpload = 0x0201,
    DnsSignature = 0x0202,
    P2pSignatures = 0x0203,
    SubmissionSignatures = 0x0204,
    KickSignatures = 0x0205,

    // Chunked Transfer (0x0300 - 0x03FF)
    /// Command with chunked payload - payload contains: [command_type (2 bytes)] [total_size (4 bytes)] [chunk_count (4 bytes)]
    ChunkedCommand = 0x0300,
    /// Request chunk N - payload contains: [chunk_index (4 bytes)]
    GetChunk = 0x0301,
    /// Chunk data response - payload contains: [chunk_index (4 bytes)] [chunk_data (variable)]
    Chunk = 0x0302,
}

/// Maximum payload size before chunking is required (1KB to stay safely under Noise frame limits)
pub const MAX_PAYLOAD_SIZE: usize = 1024;

/// Chunk size for large payloads
pub const CHUNK_SIZE: usize = 1024;

impl TryFrom<u16> for MessageType {
    type Error = anyhow::Error;

    fn try_from(value: u16) -> std::result::Result<Self, anyhow::Error> {
        match value {
            0x0001 => Ok(Self::UploadDars),
            0x0002 => Ok(Self::GenerateKeys),
            0x0003 => Ok(Self::SignDns),
            0x0004 => Ok(Self::SignP2p),
            0x0005 => Ok(Self::SignSubmissions),
            0x0006 => Ok(Self::StatusUpdate),
            0x0007 => Ok(Self::Disconnect),
            0x0008 => Ok(Self::GetNextCommand),
            0x0009 => Ok(Self::SignKick),
            0x000A => Ok(Self::Ping),
            0x000B => Ok(Self::ListPackages),
            0x000C => Ok(Self::RequestOwnerKeys),
            0x000D => Ok(Self::ListPeers),
            0x000E => Ok(Self::RequestMemberParty),
            0x0010 => Ok(Self::InviteOnboarding),
            0x0011 => Ok(Self::InviteKick),
            0x0012 => Ok(Self::InviteContracts),
            0x0013 => Ok(Self::InviteDars),
            0x0101 => Ok(Self::Ack),
            0x0102 => Ok(Self::Data),
            0x0103 => Ok(Self::Error),
            0x0104 => Ok(Self::Ready),
            0x0105 => Ok(Self::Wait),
            0x0106 => Ok(Self::Pong),
            0x0107 => Ok(Self::OwnerKeys),
            0x0108 => Ok(Self::PeerList),
            0x0109 => Ok(Self::MemberPartyResponse),
            0x0201 => Ok(Self::KeysUpload),
            0x0202 => Ok(Self::DnsSignature),
            0x0203 => Ok(Self::P2pSignatures),
            0x0204 => Ok(Self::SubmissionSignatures),
            0x0205 => Ok(Self::KickSignatures),
            0x0300 => Ok(Self::ChunkedCommand),
            0x0301 => Ok(Self::GetChunk),
            0x0302 => Ok(Self::Chunk),
            _ => Err(anyhow::anyhow!("Unknown message type: 0x{value:04x}")),
        }
    }
}

impl MessageType {
    pub fn to_u16(self) -> u16 {
        self as u16
    }
}

/// Message structure for Noise protocol communication
#[derive(Clone, Debug)]
pub struct Message {
    pub msg_type: MessageType,
    pub payload: Vec<u8>,
    _p: PhantomData<()>,
}

impl Message {
    pub fn new(msg_type: MessageType, payload: Vec<u8>) -> Self {
        Self {
            msg_type,
            payload,
            _p: PhantomData,
        }
    }

    /// Create a message with no payload
    pub fn new_empty(msg_type: MessageType) -> Self {
        Self {
            msg_type,
            payload: Vec::new(),
            _p: PhantomData,
        }
    }

    /// Encode message to wire format:
    /// [MessageType (2 bytes)] [PayloadLength (4 bytes)] [Payload (variable)]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Message type (2 bytes, big-endian)
        bytes.extend_from_slice(&self.msg_type.to_u16().to_be_bytes());

        // Payload length (4 bytes, big-endian)
        bytes.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());

        // Payload
        bytes.extend_from_slice(&self.payload);

        bytes
    }

    /// Decode message from wire format
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 6 {
            anyhow::bail!(
                "Message too short: expected at least 6 bytes, got {count}",
                count = bytes.len()
            );
        }

        // Parse message type (2 bytes)
        let msg_type_value = u16::from_be_bytes([bytes[0], bytes[1]]);
        let msg_type = MessageType::try_from(msg_type_value)?;

        // Parse payload length (4 bytes)
        let payload_len = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as usize;

        // Check if we have enough bytes for the payload
        if bytes.len() < 6 + payload_len {
            anyhow::bail!(
                "Message payload truncated: expected {payload_len} bytes, got {count}",
                count = bytes.len() - 6
            );
        }

        // Extract payload
        let payload = bytes[6..6 + payload_len].to_vec();

        Ok(Self {
            msg_type,
            payload,
            _p: PhantomData,
        })
    }
}

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

    #[error("Bad status code: {0}")]
    BadStatusCode(StatusCode),

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

/// Helper function to parse flexible URIs (with or without scheme)
pub fn parse_flexible_uri(uri_str: &str) -> Result<Uri, http::uri::InvalidUri> {
    let url = match uri_str.find("://") {
        None => format!("http://{uri_str}"),
        Some(_) => uri_str.to_string(),
    };

    url.parse::<Uri>()
}

/// Parse a hex-encoded public key string into a PublicKey
pub fn parse_public_key(hex_str: &str) -> Result<PublicKey, NoiseError> {
    let pub_key_bytes = hex::decode(hex_str).map_err(|_| NoiseError::InvalidMessage)?;
    let pub_key = PublicKey::from_slice(&pub_key_bytes).map_err(|_| NoiseError::InvalidMessage)?;
    Ok(pub_key)
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

    let request = Request::builder()
        .uri(uri)
        .method("POST")
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
        return Err(NoiseError::BadStatusCode(response.status()));
    }

    let resp_body_bytes = hyper::body::to_bytes(response.body_mut()).await?;
    Ok(resp_body_bytes)
}

/// Static keypair for Noise protocol authentication
#[derive(Clone, Debug)]
pub struct NoiseKeypair {
    pub secret_key: SecretKey,
    pub public_key: PublicKey,
}

impl NoiseKeypair {
    /// Generate a new random keypair
    pub fn generate() -> Self {
        let secp = Secp256k1::new();
        let (secret_key, public_key) = secp.generate_keypair(&mut rand::thread_rng());
        Self {
            secret_key,
            public_key,
        }
    }

    /// Load keypair from a file (expects hex-encoded secret key)
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        use anyhow::Context;

        let path = path.as_ref();
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read key file '{}'", path.display()))?;
        let secret_key_hex = content.trim();
        let secret_key_bytes = hex::decode(secret_key_hex)?;
        let secret_key = SecretKey::from_slice(&secret_key_bytes)?;
        let secp = Secp256k1::new();
        let public_key = PublicKey::from_secret_key(&secp, &secret_key);

        Ok(Self {
            secret_key,
            public_key,
        })
    }

    /// Save the private key to a file (hex-encoded)
    pub async fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directory '{}'", parent.display()))?;
        }

        let secret_key_hex = hex::encode(self.secret_key.secret_bytes());
        tokio::fs::write(path, secret_key_hex)
            .await
            .with_context(|| format!("Failed to write key file '{}'", path.display()))?;
        Ok(())
    }

    /// Get the public key as hex string
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key.serialize())
    }

    /// Derive a pre-shared key (PSK) from a peer's public key using ECDH
    pub fn derive_psk(&self, peer_public_key: &PublicKey) -> [u8; 32] {
        secp256k1::ecdh::SharedSecret::new(peer_public_key, &self.secret_key).secret_bytes()
    }
}

/// Load or generate a Noise keypair
///
/// If the key file exists, loads it. Otherwise, generates a new keypair and saves it.
/// This is the primary way to obtain a keypair for the application.
pub async fn load_or_generate_keypair<P: AsRef<Path>>(path: P) -> Result<NoiseKeypair> {
    let path = path.as_ref();

    if path.exists() {
        tracing::debug!(
            "Loading existing Noise keypair from {path}",
            path = path.display()
        );
        NoiseKeypair::from_file(path).await
    } else {
        tracing::info!("No Noise keypair found, generating new one");
        let keypair = NoiseKeypair::generate();
        keypair.save_to_file(path).await?;
        tracing::info!("Noise keypair saved to {path}", path = path.display());
        tracing::info!("Public key: {key}", key = keypair.public_key_hex());
        Ok(keypair)
    }
}

/// Returns `true` if a `NoiseError` represents a transient condition that
/// is worth retrying. Deterministic errors (handshake failures, 4xx status,
/// decode errors, configuration mistakes) are not retried; 5xx responses
/// are treated as transient (server-side hiccup).
///
/// Exhaustive match (no wildcard) — adding a new `NoiseError` variant will
/// fail to compile here until it's explicitly classified as retryable or not.
fn is_transient(err: &NoiseError) -> bool {
    match err {
        NoiseError::TcpConnectionTimeout(_)
        | NoiseError::TcpConnectionFailed(_)
        | NoiseError::RequestTimeout
        | NoiseError::Io(_)
        | NoiseError::Hyper(_) => true,
        NoiseError::BadStatusCode(code) => code.is_server_error(),
        NoiseError::Noise(_)
        | NoiseError::HandshakeFailed
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

/// Run `op` up to `config.max_attempts` times, retrying only when the returned
/// `NoiseError` is classified as transient by `is_transient`. Sleeps
/// `config.backoff()` between attempts. Per-attempt failures are logged at
/// `warn`; terminal failures (after retry exhaustion) are logged at `error`.
/// `peer_label` is used as a structured field in the log lines.
async fn retry_loop<F, Fut>(
    peer_label: &str,
    config: &NoiseRetryConfig,
    mut op: F,
) -> Result<Bytes, NoiseError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Bytes, NoiseError>>,
{
    debug_assert!(
        config.max_attempts > 0,
        "NoiseRetryConfig.max_attempts must be > 0"
    );
    let mut last_err: Option<NoiseError> = None;
    for attempt in 1..=config.max_attempts {
        match op().await {
            Ok(bytes) => return Ok(bytes),
            Err(e) if is_transient(&e) => {
                tracing::warn!(
                    peer = peer_label,
                    attempt,
                    error = %e,
                    "noise retry: transient failure, will retry",
                );
                last_err = Some(e);
                if attempt < config.max_attempts {
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
    let final_err = last_err.expect("retry_loop ran zero attempts");
    tracing::error!(
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
/// Per-attempt failures log at `tracing::warn!`; terminal failures (after
/// retry exhaustion) log an additional `tracing::error!`.
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

    fn test_retry_config() -> NoiseRetryConfig {
        NoiseRetryConfig::default()
    }

    #[test]
    fn test_message_type_conversion() -> Result {
        assert_eq!(MessageType::UploadDars.to_u16(), 0x0001);
        assert_eq!(MessageType::try_from(0x0001)?, MessageType::UploadDars);
        assert_eq!(MessageType::Ack.to_u16(), 0x0101);
        assert_eq!(MessageType::try_from(0x0101)?, MessageType::Ack);
        assert!(MessageType::try_from(0xFFFF).is_err());
        Ok(())
    }

    #[test]
    fn test_message_encoding_empty() {
        let msg = Message::new_empty(MessageType::UploadDars);
        let bytes = msg.to_bytes();

        // Should be 6 bytes: 2 for type, 4 for length (0)
        assert_eq!(bytes.len(), 6);
        assert_eq!(bytes[0..2], [0x00, 0x01]); // Type
        assert_eq!(bytes[2..6], [0x00, 0x00, 0x00, 0x00]); // Length
    }

    #[test]
    fn test_message_encoding_with_payload() {
        let payload = vec![0x01, 0x02, 0x03, 0x04];
        let msg = Message::new(MessageType::Data, payload.clone());
        let bytes = msg.to_bytes();

        // Should be 10 bytes: 2 for type, 4 for length, 4 for payload
        assert_eq!(bytes.len(), 10);
        assert_eq!(bytes[0..2], [0x01, 0x02]); // Type (Data = 0x0102)
        assert_eq!(bytes[2..6], [0x00, 0x00, 0x00, 0x04]); // Length (4)
        assert_eq!(bytes[6..10], payload[..]); // Payload
    }

    #[test]
    fn test_message_roundtrip() -> Result {
        let original = Message::new(MessageType::StatusUpdate, b"test data".to_vec());
        let bytes = original.to_bytes();
        let decoded = Message::from_bytes(&bytes)?;

        assert_eq!(decoded.msg_type, original.msg_type);
        assert_eq!(decoded.payload, original.payload);
        Ok(())
    }

    #[test]
    fn test_message_decoding_too_short() {
        let bytes = vec![0x00, 0x01]; // Only 2 bytes, need at least 6
        let result = Message::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_message_decoding_truncated_payload() {
        let mut bytes = vec![0x00, 0x01]; // Type
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0A]); // Length = 10
        bytes.extend_from_slice(&[0x01, 0x02]); // Only 2 bytes of payload

        let result = Message::from_bytes(&bytes);
        assert!(result.is_err());
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
        let result = retry_loop("test-peer", &test_retry_config(), move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, _>(NoiseError::TcpConnectionTimeout("test".into()))
            }
        })
        .await;
        assert!(matches!(result, Err(NoiseError::TcpConnectionTimeout(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retry_loop_does_not_retry_on_4xx_bad_status() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let result = retry_loop("test-peer", &test_retry_config(), move || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, _>(NoiseError::BadStatusCode(StatusCode::BAD_REQUEST))
            }
        })
        .await;
        assert!(matches!(result, Err(NoiseError::BadStatusCode(_))));
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
                Err::<Bytes, _>(NoiseError::BadStatusCode(StatusCode::INTERNAL_SERVER_ERROR))
            }
        })
        .await;
        assert!(matches!(result, Err(NoiseError::BadStatusCode(_))));
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
}
