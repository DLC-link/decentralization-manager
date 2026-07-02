pub mod client;
pub mod server;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{marker::PhantomData, path::Path, time::Duration};

use anyhow::Context;
use bytes::Bytes;
use http::Uri;
use hyper::{Body, Request, StatusCode};
use secp256k1::{PublicKey, Secp256k1, SecretKey, ecdh::SharedSecret};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_noise::handshakes::nn_psk2::Initiator;
use zeroize::Zeroizing;

use crate::{config::NoiseRetryConfig, error::Result};

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
    Health = 0x000F,
    // Add-party commands live above the invite block (0x0010-0x001F) because
    // the low command range 0x0001-0x000F is exhausted.
    /// Command: the new member generates its namespace + DAML keys. Other
    /// peers reply with a skip status.
    GenerateAddPartyKeys = 0x0020,
    /// Command: every peer signs the add-party DNS + P2P proposals.
    SignAddParty = 0x0021,
    /// Command: the new member imports the party's ACS snapshot. Other peers
    /// reply with a skip status.
    ImportAcs = 0x0022,
    /// Command: the new member drives `ClearPartyOnboardingFlag` past
    /// Canton's safe time. Other peers reply with a skip status.
    ClearOnboardingFlag = 0x0023,
    /// Command: every peer signs the onboarding-flag clearing proposal
    /// (no-op when the payload carries the empty skip marker).
    SignClearOnboarding = 0x0024,
    /// Command: every party member signs the change-threshold DNS + P2P
    /// proposals.
    SignChangeThreshold = 0x0025,

    // Invites (0x0010 - 0x001F)
    InviteOnboarding = 0x0010,
    InviteKick = 0x0011,
    InviteContracts = 0x0012,
    InviteDars = 0x0013,
    CancelInvite = 0x0014,
    /// Coordinator-initiated retry: tells peers who accepted an earlier
    /// invite from this coordinator to flip their Failed run back to
    /// InProgress and re-spin `start_peer`.
    RetryWorkflow = 0x0015,
    /// Peer-initiated decline: sent to the coordinator right before a peer
    /// removes its local pending invitation, so the coordinator's matching
    /// in-progress run can be marked Failed instead of hanging until
    /// timeout. Payload is a JSON `DeclineInvitationPayload`.
    DeclineInvitation = 0x0016,
    /// Invite to participate in adding a new member to a decentralized
    /// party. Payload is a JSON `AddPartyInvitePayload`.
    InviteAddParty = 0x0017,
    /// Invite to participate in changing a decentralized party's threshold.
    /// Payload is a JSON `ChangeThresholdInvitePayload`.
    InviteChangeThreshold = 0x0018,

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
    HealthResponse = 0x010A,
    Busy = 0x010B,

    // Data Transfers (0x0200 - 0x02FF)
    KeysUpload = 0x0201,
    DnsSignature = 0x0202,
    P2pSignatures = 0x0203,
    SubmissionSignatures = 0x0204,
    KickSignatures = 0x0205,
    /// New member → coordinator: `keys||participant_id` payload.
    AddPartyKeysUpload = 0x0206,
    /// Peer → coordinator: signed add-party DNS + P2P pair.
    AddPartySignatures = 0x0207,
    /// Peer → coordinator: signed onboarding-flag clearing proposal.
    AddPartyClearSignatures = 0x0208,
    /// New member → coordinator: the clearing proposal it AUTHORED. Canton
    /// requires the onboarding participant itself to issue the flag-clear
    /// transaction, so the new member authors it and the coordinator only
    /// runs the signing round on it.
    AddPartyClearProposal = 0x0209,
    /// Peer → coordinator: signed change-threshold DNS + P2P pair.
    ChangeThresholdSignatures = 0x020A,

    // Chunked Transfer (0x0300 - 0x03FF)
    /// Command with chunked payload - payload contains: [command_type (2 bytes)] [total_size (4 bytes)] [chunk_count (4 bytes)]
    ChunkedCommand = 0x0300,
    /// Request chunk N - payload contains: [chunk_index (4 bytes)]
    GetChunk = 0x0301,
    /// Chunk data response - payload contains: [chunk_index (4 bytes)] [chunk_data (variable)]
    Chunk = 0x0302,
}

/// Maximum payload size sent inline in a single message before the chunked
/// fetch protocol kicks in. Matched to `CHUNK_SIZE` so any payload that fits
/// in one chunk is delivered in a single round-trip; larger payloads are
/// split into `CHUNK_SIZE` chunks. The `tokio-noise` transport streams and
/// auto-fragments writes, so this is not constrained by the Noise frame size.
pub const MAX_PAYLOAD_SIZE: usize = 1024 * 1024;

/// Chunk size for large payloads (1 MiB).
///
/// Each chunk is fetched in its own Noise call (a fresh TCP connect, handshake,
/// and one round-trip), so fewer and larger chunks mean far fewer round-trips
/// over high-latency links. The `tokio-noise` transport streams writes and
/// auto-fragments them into ~2 KiB frames, so this is not bound by the Noise
/// frame size — only by `MAX_CHUNKED_TOTAL_SIZE`.
pub const CHUNK_SIZE: usize = 1024 * 1024;

/// Hard ceiling on the assembled size of a chunked response. Bounds peer-supplied
/// `total_size` so a malicious or buggy peer can't ask the client to allocate
/// arbitrary memory. 16 MiB is well above any plausible `ListPackages` payload
/// (SV nodes observed at ~8 KiB) while keeping worst-case allocation bounded.
pub const MAX_CHUNKED_TOTAL_SIZE: usize = 16 * 1024 * 1024;

/// Hard ceiling on the chunk count for a chunked response. Equal to
/// `MAX_CHUNKED_TOTAL_SIZE / CHUNK_SIZE` rounded up.
pub const MAX_CHUNK_COUNT: usize = MAX_CHUNKED_TOTAL_SIZE.div_ceil(CHUNK_SIZE);

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
            0x000F => Ok(Self::Health),
            0x0010 => Ok(Self::InviteOnboarding),
            0x0011 => Ok(Self::InviteKick),
            0x0012 => Ok(Self::InviteContracts),
            0x0013 => Ok(Self::InviteDars),
            0x0014 => Ok(Self::CancelInvite),
            0x0015 => Ok(Self::RetryWorkflow),
            0x0016 => Ok(Self::DeclineInvitation),
            0x0017 => Ok(Self::InviteAddParty),
            0x0020 => Ok(Self::GenerateAddPartyKeys),
            0x0021 => Ok(Self::SignAddParty),
            0x0022 => Ok(Self::ImportAcs),
            0x0023 => Ok(Self::ClearOnboardingFlag),
            0x0024 => Ok(Self::SignClearOnboarding),
            0x0025 => Ok(Self::SignChangeThreshold),
            0x0018 => Ok(Self::InviteChangeThreshold),
            0x0101 => Ok(Self::Ack),
            0x0102 => Ok(Self::Data),
            0x0103 => Ok(Self::Error),
            0x0104 => Ok(Self::Ready),
            0x0105 => Ok(Self::Wait),
            0x0106 => Ok(Self::Pong),
            0x0107 => Ok(Self::OwnerKeys),
            0x0108 => Ok(Self::PeerList),
            0x0109 => Ok(Self::MemberPartyResponse),
            0x010A => Ok(Self::HealthResponse),
            0x010B => Ok(Self::Busy),
            0x0201 => Ok(Self::KeysUpload),
            0x0202 => Ok(Self::DnsSignature),
            0x0203 => Ok(Self::P2pSignatures),
            0x0204 => Ok(Self::SubmissionSignatures),
            0x0205 => Ok(Self::KickSignatures),
            0x0206 => Ok(Self::AddPartyKeysUpload),
            0x0207 => Ok(Self::AddPartySignatures),
            0x0208 => Ok(Self::AddPartyClearSignatures),
            0x0209 => Ok(Self::AddPartyClearProposal),
            0x020A => Ok(Self::ChangeThresholdSignatures),
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

/// Noise application-frame protocol version. First byte of every frame, so a
/// peer on an incompatible build is detected explicitly instead of surfacing
/// as garbled length/UTF-8 parse errors. Chosen >= 0x04 because legacy
/// (pre-version-byte) frames started with a `MessageType` whose high byte is
/// always 0x00..=0x03 — an old frame can never alias a valid version.
/// Bump this on any framing change; the decoder rejects mismatches with a
/// clear error, and future versions can branch on it instead of forcing
/// another lockstep upgrade.
pub const WIRE_VERSION: u8 = 0xD1;

/// Message structure for Noise protocol communication.
///
/// `instance` carries the coordinator's workflow `instance_name` so the
/// always-on listener can route a peer's workflow-command traffic
/// (`GetNextCommand`, signatures, …) to the correct concurrent run when a node
/// coordinates more than one workflow at a time. It is empty for non-workflow
/// requests (`ListPeers`, `RequestOwnerKeys`, …) and for invite/control
/// messages, which the listener dispatches without per-instance routing. The
/// peer learns the value from the invite's `workflow_instance` field and
/// echoes it on every subsequent command.
#[derive(Clone, Debug)]
pub struct Message {
    pub msg_type: MessageType,
    pub instance: String,
    pub payload: Vec<u8>,
    _p: PhantomData<()>,
}

impl Message {
    pub fn new(msg_type: MessageType, payload: Vec<u8>) -> Self {
        Self {
            msg_type,
            instance: String::new(),
            payload,
            _p: PhantomData,
        }
    }

    /// Create a message with no payload
    pub fn new_empty(msg_type: MessageType) -> Self {
        Self {
            msg_type,
            instance: String::new(),
            payload: Vec::new(),
            _p: PhantomData,
        }
    }

    /// Set the routing `instance_name` (the coordinator's run identifier) so
    /// the always-on listener can dispatch this command to the right run.
    #[must_use]
    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = instance.into();
        self
    }

    /// Encode message to wire format:
    /// `[Version (1)] [MessageType (2)] [InstanceLen (2)] [Instance] [PayloadLength (4)] [Payload]`
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode_with_instance(&self.instance)
    }

    /// Encode with `instance` substituted for the message's own routing field
    /// — lets `NoiseClient` stamp its per-run instance without cloning the
    /// (potentially chunk-sized) payload first.
    pub fn encode_with_instance(&self, instance: &str) -> Vec<u8> {
        let instance_bytes = instance.as_bytes();
        let mut bytes = Vec::with_capacity(9 + instance_bytes.len() + self.payload.len());

        // Protocol version (1 byte)
        bytes.push(WIRE_VERSION);

        // Message type (2 bytes, big-endian)
        bytes.extend_from_slice(&self.msg_type.to_u16().to_be_bytes());

        // Routing instance: length (2 bytes, big-endian) + UTF-8 bytes.
        // Instance names are workflow `instance_name`s (validated party prefix +
        // kind + timestamp), always far below the 16-bit length ceiling. Enforce
        // it in ALL builds, not just debug: the instance can originate from a
        // peer's invite, so a `len() as u16` truncation would emit an
        // undecodable frame (and the value is attacker-influenceable). If it
        // ever exceeds the ceiling, log loudly and emit an EMPTY instance — the
        // frame stays valid; routing simply misses → 503 → bounded retry —
        // rather than corrupting the stream or panicking the encoder.
        let instance_bytes: &[u8] = if instance_bytes.len() > u16::MAX as usize {
            tracing::error!(
                "Message.instance is {} bytes (> {}); dropping the routing key to keep the \
                 frame decodable",
                instance_bytes.len(),
                u16::MAX
            );
            &[]
        } else {
            instance_bytes
        };
        bytes.extend_from_slice(&(instance_bytes.len() as u16).to_be_bytes());
        bytes.extend_from_slice(instance_bytes);

        // Payload length (4 bytes, big-endian)
        bytes.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());

        // Payload
        bytes.extend_from_slice(&self.payload);

        bytes
    }

    /// Decode message from wire format
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        // Minimum: version (1) + type (2) + instance_len (2) + payload_len (4).
        if bytes.len() < 9 {
            anyhow::bail!(
                "Message too short: expected at least 9 bytes, got {count}",
                count = bytes.len()
            );
        }

        // Protocol version FIRST, so an incompatible peer surfaces as exactly
        // that — not as a garbled-length or UTF-8 parse error downstream.
        // Legacy (pre-version) frames start with a MessageType high byte of
        // 0x00..=0x03, which can never equal WIRE_VERSION.
        if bytes[0] != WIRE_VERSION {
            anyhow::bail!(
                "Noise protocol version mismatch: got 0x{got:02x}, expected 0x{want:02x} — \
                 the peer is running an incompatible dec-party-manager build",
                got = bytes[0],
                want = WIRE_VERSION
            );
        }

        // Parse message type (2 bytes)
        let msg_type_value = u16::from_be_bytes([bytes[1], bytes[2]]);
        let msg_type = MessageType::try_from(msg_type_value)?;

        // Parse routing instance length (2 bytes) + bytes
        let instance_len = u16::from_be_bytes([bytes[3], bytes[4]]) as usize;
        let instance_end = 5 + instance_len;
        if bytes.len() < instance_end + 4 {
            anyhow::bail!(
                "Message instance truncated: expected {instance_len} instance bytes + 4 length \
                 bytes, got {count} after the header",
                count = bytes.len().saturating_sub(5)
            );
        }
        let instance = String::from_utf8(bytes[5..instance_end].to_vec())
            .map_err(|e| anyhow::anyhow!("Message instance is not valid UTF-8: {e}"))?;

        // Parse payload length (4 bytes)
        let len_start = instance_end;
        let payload_len = u32::from_be_bytes([
            bytes[len_start],
            bytes[len_start + 1],
            bytes[len_start + 2],
            bytes[len_start + 3],
        ]) as usize;
        let payload_start = len_start + 4;

        // Check if we have enough bytes for the payload
        if bytes.len() < payload_start + payload_len {
            anyhow::bail!(
                "Message payload truncated: expected {payload_len} bytes, got {count}",
                count = bytes.len() - payload_start
            );
        }

        // Extract payload
        let payload = bytes[payload_start..payload_start + payload_len].to_vec();

        Ok(Self {
            msg_type,
            instance,
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

/// Owned, zeroizing secp256k1 secret-key bytes.
///
/// Validated against `SecretKey::from_slice` on construction so subsequent
/// conversions back to a `SecretKey` cannot fail. The inner bytes live in
/// `Zeroizing<[u8; 32]>` and are wiped on drop. Intentionally no `Clone`,
/// `Copy`, `Debug`, or `Serialize` — secret access goes through the methods
/// on `NoiseKeypair`.
pub struct NoiseSecretKey(Zeroizing<[u8; 32]>);

impl NoiseSecretKey {
    fn from_bytes(bytes: [u8; 32]) -> Result<Self> {
        // Validate via secp256k1; the constructed SecretKey is dropped here.
        SecretKey::from_slice(&bytes)?;
        Ok(Self(Zeroizing::new(bytes)))
    }

    fn to_secp_secret_key(&self) -> SecretKey {
        // Bytes were validated on construction; reconstruction is infallible.
        SecretKey::from_slice(&self.0[..])
            .expect("NoiseSecretKey bytes were validated on construction")
    }

    fn as_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(hex::encode(&self.0[..]))
    }
}

/// Static keypair for Noise protocol authentication
pub struct NoiseKeypair {
    secret_key: NoiseSecretKey,
    pub public_key: PublicKey,
}

impl NoiseKeypair {
    /// Generate a new random keypair
    pub fn generate() -> Self {
        let secp = Secp256k1::new();
        let (secret_key, public_key) = secp.generate_keypair(&mut secp256k1::rand::thread_rng());
        let bytes = secret_key.secret_bytes();
        Self {
            secret_key: NoiseSecretKey::from_bytes(bytes)
                .expect("freshly-generated secp256k1 secret key is always valid"),
            public_key,
        }
    }

    /// Load keypair from a file (expects hex-encoded secret key)
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        // Tighten permissions BEFORE reading so any existing overly-permissive
        // file is locked down before we leak its bytes onto our heap. Also
        // runs here (not just in `load_or_generate_keypair`) so every other
        // caller — workflow handlers, noise/client.rs, noise/server.rs — picks
        // up the chmod even on the direct `from_file` path.
        #[cfg(unix)]
        ensure_key_file_permissions(path).await?;
        // Read as raw bytes into a zeroizing buffer so the hex representation
        // never lives in a non-zeroizing `String`. The decoded 32-byte secret
        // is also held in a zeroizing buffer before being moved into
        // `NoiseSecretKey`.
        let raw = Zeroizing::new(
            tokio::fs::read(path)
                .await
                .with_context(|| format!("Failed to read key file '{}'", path.display()))?,
        );
        let hex_str = std::str::from_utf8(&raw)
            .with_context(|| format!("Key file '{}' is not UTF-8", path.display()))?
            .trim();
        let decoded = Zeroizing::new(hex::decode(hex_str)?);
        let bytes: [u8; 32] = decoded.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "Key file '{}' did not contain 32 bytes of hex",
                path.display()
            )
        })?;
        let secret_key = NoiseSecretKey::from_bytes(bytes)?;
        let secp = Secp256k1::new();
        let public_key = PublicKey::from_secret_key(&secp, &secret_key.to_secp_secret_key());
        Ok(Self {
            secret_key,
            public_key,
        })
    }

    /// Save the private key to a file (hex-encoded)
    ///
    /// On unix, the file is created with mode 0600 atomically — no window
    /// in which the file is readable to other users.
    pub async fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result {
        let path = path.as_ref();

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directory '{}'", parent.display()))?;
        }

        let hex_string = self.secret_key.as_hex();

        #[cfg(unix)]
        {
            // `OpenOptions::mode` only applies on create — when the target
            // file already exists, its prior permissions are preserved.
            // Tighten to 0600 first so the truncate-and-write below cannot
            // leave the new key bytes briefly readable to other users.
            if path.exists() {
                tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .await
                    .with_context(|| {
                        format!("Failed to chmod existing key file '{}'", path.display())
                    })?;
            }
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .await
                .with_context(|| format!("Failed to open key file '{}'", path.display()))?;
            file.write_all(hex_string.as_bytes())
                .await
                .with_context(|| format!("Failed to write key file '{}'", path.display()))?;
            file.sync_all()
                .await
                .with_context(|| format!("Failed to sync key file '{}'", path.display()))?;
        }
        #[cfg(not(unix))]
        {
            tokio::fs::write(path, hex_string.as_bytes())
                .await
                .with_context(|| format!("Failed to write key file '{}'", path.display()))?;
        }

        Ok(())
    }

    /// Get the public key as hex string
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key.serialize())
    }

    /// Derive a pre-shared key (PSK) from a peer's public key using ECDH
    pub fn derive_psk(&self, peer_public_key: &PublicKey) -> Zeroizing<[u8; 32]> {
        let secp_sk = self.secret_key.to_secp_secret_key();
        Zeroizing::new(SharedSecret::new(peer_public_key, &secp_sk).secret_bytes())
    }
}

/// Ensure the key file has restrictive (0600) permissions.
///
/// Idempotent: runs on every load so existing keys deployed with a more
/// permissive default are tightened on the next startup. Emits one
/// `warn!` when it has to change anything, so operators can confirm
/// which nodes had the pre-fix permissions.
#[cfg(unix)]
async fn ensure_key_file_permissions(path: &Path) -> Result {
    let metadata = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("Failed to stat key file '{}'", path.display()))?;
    let current_mode = metadata.permissions().mode() & 0o777;
    if current_mode != 0o600 {
        tracing::warn!(
            "Tightening permissions on key file {path} from {current_mode:o} to 0600",
            path = path.display(),
        );
        let mut perms = metadata.permissions();
        perms.set_mode(0o600);
        tokio::fs::set_permissions(path, perms)
            .await
            .with_context(|| format!("Failed to chmod key file '{}'", path.display()))?;
    }
    Ok(())
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
        // `from_file` itself calls `ensure_key_file_permissions` first, so
        // every caller (not just this helper) gets the chmod-on-load.
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
pub(crate) fn is_transient(err: &NoiseError) -> bool {
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

    fn test_retry_config() -> NoiseRetryConfig {
        // Zero backoff so the `retry_loop_*` tests don't sleep in real time.
        // (Tests that need to assert on backoff behavior should override.)
        NoiseRetryConfig {
            backoff_ms: 0,
            ..NoiseRetryConfig::default()
        }
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
    fn add_party_message_types_round_trip() -> Result {
        for mt in [
            MessageType::GenerateAddPartyKeys,
            MessageType::SignAddParty,
            MessageType::ImportAcs,
            MessageType::ClearOnboardingFlag,
            MessageType::SignClearOnboarding,
            MessageType::InviteAddParty,
            MessageType::AddPartyKeysUpload,
            MessageType::AddPartySignatures,
            MessageType::AddPartyClearSignatures,
            MessageType::AddPartyClearProposal,
        ] {
            assert_eq!(MessageType::try_from(mt.to_u16())?, mt);
        }
        Ok(())
    }

    #[test]
    fn change_threshold_message_types_round_trip() -> Result {
        for mt in [
            MessageType::SignChangeThreshold,
            MessageType::InviteChangeThreshold,
            MessageType::ChangeThresholdSignatures,
        ] {
            assert_eq!(MessageType::try_from(mt.to_u16())?, mt);
        }
        Ok(())
    }

    #[test]
    fn health_message_types_round_trip() -> Result {
        for mt in [
            MessageType::Health,
            MessageType::HealthResponse,
            MessageType::Busy,
        ] {
            assert_eq!(MessageType::try_from(mt.to_u16())?, mt);
        }
        // Health request encodes/decodes through the wire format.
        let decoded = Message::from_bytes(&Message::new_empty(MessageType::Health).to_bytes())?;
        assert_eq!(decoded.msg_type, MessageType::Health);
        Ok(())
    }

    #[test]
    fn test_message_encoding_empty() {
        let msg = Message::new_empty(MessageType::UploadDars);
        let bytes = msg.to_bytes();

        // 9 bytes: 1 version, 2 type, 2 instance_len (0), 4 payload_len (0).
        assert_eq!(bytes.len(), 9);
        assert_eq!(bytes[0], WIRE_VERSION);
        assert_eq!(bytes[1..3], [0x00, 0x01]); // Type
        assert_eq!(bytes[3..5], [0x00, 0x00]); // Instance length (0)
        assert_eq!(bytes[5..9], [0x00, 0x00, 0x00, 0x00]); // Payload length (0)
    }

    #[test]
    fn test_message_encoding_with_payload() {
        let payload = vec![0x01, 0x02, 0x03, 0x04];
        let msg = Message::new(MessageType::Data, payload.clone());
        let bytes = msg.to_bytes();

        // 13 bytes: 1 version, 2 type, 2 instance_len (0), 4 payload_len, 4 payload.
        assert_eq!(bytes.len(), 13);
        assert_eq!(bytes[0], WIRE_VERSION);
        assert_eq!(bytes[1..3], [0x01, 0x02]); // Type (Data = 0x0102)
        assert_eq!(bytes[3..5], [0x00, 0x00]); // Instance length (0)
        assert_eq!(bytes[5..9], [0x00, 0x00, 0x00, 0x04]); // Payload length (4)
        assert_eq!(bytes[9..13], payload[..]); // Payload
    }

    #[test]
    fn test_message_rejects_version_mismatch() {
        // A legacy (pre-version-byte) frame: starts with the MessageType high
        // byte (0x00..=0x03) where the version now lives. The decoder must
        // name the real problem instead of a confusing downstream parse error.
        let mut legacy = vec![0x00, 0x01]; // old Type
        legacy.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // old payload_len
        legacy.extend_from_slice(&[0x00, 0x00, 0x00]); // padding to pass min-length
        let err = match Message::from_bytes(&legacy) {
            Err(e) => format!("{e}"),
            Ok(_) => String::new(),
        };
        assert!(
            err.contains("version mismatch"),
            "expected a version-mismatch error, got: {err}"
        );
    }

    #[test]
    fn test_encode_with_instance_substitutes_routing_key() {
        // The clone-free stamping path used by NoiseClient must produce the
        // same frame as stamping via with_instance + to_bytes.
        let msg = Message::new(MessageType::GetChunk, vec![0, 0, 0, 7]);
        let direct = msg.encode_with_instance("run-x");
        let via_clone = msg.clone().with_instance("run-x").to_bytes();
        assert_eq!(direct, via_clone);
    }

    #[test]
    fn test_message_roundtrip() -> Result {
        let original = Message::new(MessageType::StatusUpdate, b"test data".to_vec());
        let bytes = original.to_bytes();
        let decoded = Message::from_bytes(&bytes)?;

        assert_eq!(decoded.msg_type, original.msg_type);
        assert_eq!(decoded.instance, "");
        assert_eq!(decoded.payload, original.payload);
        Ok(())
    }

    #[test]
    fn test_message_roundtrip_with_instance() -> Result {
        // The routing instance must survive a wire round-trip so the always-on
        // listener can dispatch concurrent same-kind workflows correctly.
        let original = Message::new(MessageType::GetNextCommand, b"".to_vec())
            .with_instance("onboarding-acme-1717000000");
        let bytes = original.to_bytes();
        let decoded = Message::from_bytes(&bytes)?;

        assert_eq!(decoded.msg_type, MessageType::GetNextCommand);
        assert_eq!(decoded.instance, "onboarding-acme-1717000000");
        assert!(decoded.payload.is_empty());
        Ok(())
    }

    #[test]
    fn test_message_roundtrip_instance_and_payload() -> Result {
        let original =
            Message::new(MessageType::KeysUpload, b"keybytes".to_vec()).with_instance("kick-xyz");
        let decoded = Message::from_bytes(&original.to_bytes())?;

        assert_eq!(decoded.instance, "kick-xyz");
        assert_eq!(decoded.payload, b"keybytes");
        Ok(())
    }

    #[test]
    fn test_message_decoding_too_short() {
        let bytes = vec![WIRE_VERSION, 0x01]; // Only 2 bytes, need at least 9
        let result = Message::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_message_decoding_truncated_payload() {
        let mut bytes = vec![WIRE_VERSION];
        bytes.extend_from_slice(&[0x00, 0x01]); // Type
        bytes.extend_from_slice(&[0x00, 0x00]); // Instance length (0)
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0A]); // Payload length = 10
        bytes.extend_from_slice(&[0x01, 0x02]); // Only 2 bytes of payload

        let result = Message::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_message_decoding_truncated_instance() {
        let mut bytes = vec![WIRE_VERSION];
        bytes.extend_from_slice(&[0x00, 0x01]); // Type
        bytes.extend_from_slice(&[0x00, 0x10]); // Instance length = 16
        bytes.extend_from_slice(b"short"); // but only 5 instance bytes follow

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

    // Compile-time guard: the secret-bearing types must not expose Debug,
    // Clone, or Copy. Removing any of these breaks the security invariant.
    static_assertions::assert_not_impl_any!(NoiseKeypair: std::fmt::Debug, Clone, Copy);
    static_assertions::assert_not_impl_any!(NoiseSecretKey: std::fmt::Debug, Clone, Copy);

    #[cfg(unix)]
    #[tokio::test]
    async fn save_to_file_creates_0600() -> Result {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("noise.key");

        let keypair = NoiseKeypair::generate();
        keypair.save_to_file(&path).await?;

        let mode = tokio::fs::metadata(&path).await?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_to_file_overwrites_existing_with_0600() -> Result {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("noise.key");

        // Pre-create the file with 0644 permissions and stale contents
        tokio::fs::write(&path, b"stale-key-material").await?;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).await?;
        assert_eq!(
            tokio::fs::metadata(&path).await?.permissions().mode() & 0o777,
            0o644,
        );

        let keypair = NoiseKeypair::generate();
        keypair.save_to_file(&path).await?;

        let mode = tokio::fs::metadata(&path).await?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
        let written = tokio::fs::read_to_string(&path).await?;
        assert_eq!(written.len(), 64, "expected 32-byte hex string");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn load_or_generate_keypair_tightens_permissions() -> Result {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("noise.key");

        // Seed a valid key file, then loosen perms to 0644
        let initial = NoiseKeypair::generate();
        initial.save_to_file(&path).await?;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).await?;
        assert_eq!(
            tokio::fs::metadata(&path).await?.permissions().mode() & 0o777,
            0o644,
        );

        let loaded = load_or_generate_keypair(&path).await?;
        assert_eq!(loaded.public_key_hex(), initial.public_key_hex());

        let mode = tokio::fs::metadata(&path).await?.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "expected chmod-on-load to tighten to 0600, got {mode:o}"
        );
        Ok(())
    }
}
