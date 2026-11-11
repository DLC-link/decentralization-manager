use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Declare `MessageType` with its wire codes and derive `TryFrom<u16>` from
/// the same list, so a code is written once.
macro_rules! wire_codes {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident = $code:literal,
            )+
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                $variant = $code,
            )+
        }

        impl TryFrom<u16> for $name {
            type Error = anyhow::Error;

            fn try_from(value: u16) -> std::result::Result<Self, anyhow::Error> {
                match value {
                    $($code => Ok(Self::$variant),)+
                    _ => Err(anyhow::anyhow!("Unknown message type: 0x{value:04x}")),
                }
            }
        }
    };
}

wire_codes! {
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
        RequestOwnerKeys = 0x000C,
        ListPeers = 0x000D,
        RequestMemberParty = 0x000E,
        Health = 0x000F,
        // Add-party commands live above the invite block (0x0010-0x001F) because
        // the low command range 0x0001-0x000F is exhausted.
        /// Command: the new member generates its namespace + Daml keys. Other
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
        /// Members authorize removal of shadowed legacy keys after topology migration.
        RetireLegacyKeys = 0x0026,
        /// Peer → coordinator: send me one DAR of a pending Dars invite, so the
        /// operator can read it before accepting. Payload is a JSON
        /// `RequestDarPayload`; the answer is the DAR's bytes, chunked when they
        /// outgrow a frame. Only a peer the run invited is served.
        RequestDar = 0x0027,

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
        /// Request the next block of the source's open ACS export - payload contains: [seq (8 bytes)]
        GetNextAcsBlock = 0x0305,
        /// ACS block response - payload contains: [seq (8 bytes)] [bytes (variable)]
        AcsBlock = 0x0306,
        /// Final ACS block - payload contains: [seq (8 bytes)] [total (8 bytes)] [sha256 hex]
        AcsBlockEnd = 0x0307,
    }
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
/// arbitrary memory. A DAR that a peer reads before it accepts is the largest
/// payload served this way; the server refuses one above this cap.
pub const MAX_CHUNKED_TOTAL_SIZE: usize = 16 * 1024 * 1024;

/// Hard ceiling on the chunk count for a chunked response. Equal to
/// `MAX_CHUNKED_TOTAL_SIZE / CHUNK_SIZE` rounded up.
pub const MAX_CHUNK_COUNT: usize = MAX_CHUNKED_TOTAL_SIZE.div_ceil(CHUNK_SIZE);

/// Bytes served per `GetNextAcsBlock`. Default; the live value is read via
/// [`acs_block_size`].
///
/// Bounded from above by `NOISE_ACS_BLOCK_TIMEOUT`, the *client's* per-block
/// budget, which is the binding one — the server's `NOISE_HANDLER_TIMEOUT` is
/// only a backstop behind it. A block that cannot transfer inside that budget
/// times out identically on every retry, so the block size implies a floor on
/// usable bandwidth: 4 MiB in 40s is ~0.84 Mbit/s, and the 8 MiB ceiling below
/// is ~1.7 Mbit/s. Bounded from below by round trips: the fixed per-block cost
/// is a TCP connect plus a Noise handshake (measured at 0.30 ms) plus two RTTs,
/// so on a high-latency link fewer, larger blocks finish sooner.
pub const ACS_BLOCK_SIZE: usize = 4 * 1024 * 1024;

/// Ceiling for [`acs_block_size`], set by what fits in
/// `NOISE_ACS_BLOCK_TIMEOUT` on a link we are willing to call usable
/// (~1.7 Mbit/s). Deliberately below `MAX_CHUNKED_TOTAL_SIZE`: that constant
/// bounds an in-memory assembly, not a per-request transfer budget.
pub const MAX_ACS_BLOCK_SIZE: usize = 8 * 1024 * 1024;

/// Bytes to request per ACS block, overridable via `DECPM_ACS_BLOCK_BYTES`.
/// Clamped to `[64 KiB, MAX_ACS_BLOCK_SIZE]` so a mistyped value cannot stall
/// every block against `NOISE_ACS_BLOCK_TIMEOUT` or shrink to a per-byte round
/// trip.
pub fn acs_block_size() -> usize {
    std::env::var("DECPM_ACS_BLOCK_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(ACS_BLOCK_SIZE)
        .clamp(64 * 1024, MAX_ACS_BLOCK_SIZE)
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
/// The byte must only ever increase: `NoiseError::WireVersionMismatch` and the
/// peer status tooltip compare bytes to say which side runs the older build.
///
/// 0xD1 -> 0xD2: the `ImportAcs` payload dropped the inline snapshot for
/// `[config, package_ids]` and the ACS moved to `GetNextAcsBlock`. This one had
/// to be a version bump rather than a tolerated difference: a 0xD1 peer decodes
/// the new two-item payload as its own legacy `[config, snapshot]`, so it would
/// read the package-id list as ACS bytes, find them non-empty, skip the package
/// preflight, and DISCONNECT its participant to import them — after the
/// topology change is already live. Rejecting the frame outright keeps an old
/// build from ever reaching that, and it fails the invite's health probe, so
/// the run does not start at all.
/// 0xD2 -> 0xD3: Contracts invitations now require deployment-intent
/// verification, and membership workflows retire legacy keys. Old peers must
/// not silently ignore the commitments and contribute unchecked signatures.
/// The version byte rejects both old invitations and old command streams,
/// including retries of persisted runs; health preflight rejects mixed meshes.
pub const WIRE_VERSION: u8 = 0xD3;

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
                "Message.instance is {len} bytes (> {max}); dropping the routing key to keep the \
                 frame decodable",
                len = instance_bytes.len(),
                max = u16::MAX
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
    ///
    /// # Errors
    ///
    /// Returns an error if `bytes` is shorter than the 9-byte header or does
    /// not start with this build's `WIRE_VERSION`. Returns an error if the
    /// message type is unknown or the routing instance is not valid UTF-8.
    /// Returns an error if the instance or the payload is cut short.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_type_conversion() -> Result {
        assert_eq!(MessageType::UploadDars.to_u16(), 0x0001);
        assert_eq!(MessageType::try_from(0x0001)?, MessageType::UploadDars);
        assert_eq!(MessageType::Ack.to_u16(), 0x0101);
        assert_eq!(MessageType::try_from(0x0101)?, MessageType::Ack);
        assert!(MessageType::try_from(0xFFFF).is_err());
        Ok(())
    }

    /// Every wire code, pinned. Peers on other builds decode these numbers,
    /// so none may change.
    const WIRE_CODES: &[(MessageType, u16)] = &[
        (MessageType::UploadDars, 0x0001),
        (MessageType::GenerateKeys, 0x0002),
        (MessageType::SignDns, 0x0003),
        (MessageType::SignP2p, 0x0004),
        (MessageType::SignSubmissions, 0x0005),
        (MessageType::StatusUpdate, 0x0006),
        (MessageType::Disconnect, 0x0007),
        (MessageType::GetNextCommand, 0x0008),
        (MessageType::SignKick, 0x0009),
        (MessageType::Ping, 0x000A),
        (MessageType::RequestOwnerKeys, 0x000C),
        (MessageType::ListPeers, 0x000D),
        (MessageType::RequestMemberParty, 0x000E),
        (MessageType::Health, 0x000F),
        (MessageType::GenerateAddPartyKeys, 0x0020),
        (MessageType::SignAddParty, 0x0021),
        (MessageType::ImportAcs, 0x0022),
        (MessageType::ClearOnboardingFlag, 0x0023),
        (MessageType::SignClearOnboarding, 0x0024),
        (MessageType::SignChangeThreshold, 0x0025),
        (MessageType::RetireLegacyKeys, 0x0026),
        (MessageType::RequestDar, 0x0027),
        (MessageType::InviteOnboarding, 0x0010),
        (MessageType::InviteKick, 0x0011),
        (MessageType::InviteContracts, 0x0012),
        (MessageType::InviteDars, 0x0013),
        (MessageType::CancelInvite, 0x0014),
        (MessageType::RetryWorkflow, 0x0015),
        (MessageType::DeclineInvitation, 0x0016),
        (MessageType::InviteAddParty, 0x0017),
        (MessageType::InviteChangeThreshold, 0x0018),
        (MessageType::Ack, 0x0101),
        (MessageType::Data, 0x0102),
        (MessageType::Error, 0x0103),
        (MessageType::Ready, 0x0104),
        (MessageType::Wait, 0x0105),
        (MessageType::Pong, 0x0106),
        (MessageType::OwnerKeys, 0x0107),
        (MessageType::PeerList, 0x0108),
        (MessageType::MemberPartyResponse, 0x0109),
        (MessageType::HealthResponse, 0x010A),
        (MessageType::Busy, 0x010B),
        (MessageType::KeysUpload, 0x0201),
        (MessageType::DnsSignature, 0x0202),
        (MessageType::P2pSignatures, 0x0203),
        (MessageType::SubmissionSignatures, 0x0204),
        (MessageType::KickSignatures, 0x0205),
        (MessageType::AddPartyKeysUpload, 0x0206),
        (MessageType::AddPartySignatures, 0x0207),
        (MessageType::AddPartyClearSignatures, 0x0208),
        (MessageType::AddPartyClearProposal, 0x0209),
        (MessageType::ChangeThresholdSignatures, 0x020A),
        (MessageType::ChunkedCommand, 0x0300),
        (MessageType::GetChunk, 0x0301),
        (MessageType::Chunk, 0x0302),
        (MessageType::GetNextAcsBlock, 0x0305),
        (MessageType::AcsBlock, 0x0306),
        (MessageType::AcsBlockEnd, 0x0307),
    ];

    #[test]
    fn every_message_type_decodes_from_its_own_code() -> Result {
        for (message_type, code) in WIRE_CODES {
            assert_eq!(message_type.to_u16(), *code, "{message_type:?}");
            assert_eq!(MessageType::try_from(*code)?, *message_type, "0x{code:04x}");
        }
        Ok(())
    }

    #[test]
    fn every_other_code_is_refused() {
        for value in 0..=u16::MAX {
            if WIRE_CODES.iter().any(|(_, code)| *code == value) {
                continue;
            }
            match MessageType::try_from(value) {
                Ok(message_type) => panic!("0x{value:04x} decoded as {message_type:?}"),
                Err(e) => assert_eq!(
                    e.to_string(),
                    format!("Unknown message type: 0x{value:04x}")
                ),
            }
        }
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
    fn rejects_peers_without_deployment_intent_enforcement() {
        for command in [
            MessageType::Health,
            MessageType::InviteContracts,
            MessageType::SignSubmissions,
        ] {
            let mut frame = Message::new_empty(command).to_bytes();
            frame[0] = 0xD2;
            assert!(
                Message::from_bytes(&frame).is_err(),
                "accepted an old {command:?} frame"
            );
        }
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
}
