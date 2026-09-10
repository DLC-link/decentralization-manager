//! Wire DTOs shared between the `decman` server and the `decman-cli` client.
//!
//! These are pure data-transfer types: they carry no server-only dependencies
//! (no sqlx/tonic/actix). The OpenAPI (`utoipa`) schema derives are gated behind
//! the `openapi` feature so dependency-light clients don't inherit them — see
//! the `cfg_attr` pattern used throughout and in [`crate::canton_id`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

/// Participant permission level
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Submission,
    Confirmation,
    Observation,
    Unknown,
}

impl Permission {
    /// Lowercase label, matching the serde wire representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Submission => "submission",
            Self::Confirmation => "confirmation",
            Self::Observation => "observation",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Participant in a decentralized party
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ParticipantInfo {
    pub participant_uid: CantonId,
    pub permission: Permission,
    /// Namespace key fingerprint for this participant (if they are an owner)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_key: Option<String>,
}

/// Contract information surfaced in the dec_party detail view.
///
/// `template_id` is the short `Module.Path:Entity` form (NOT the fully
/// qualified package_id-prefixed form). `package_name` is the human-readable
/// Daml package name (from verbose ACS); `package_version` is joined in from
/// the participant Admin API's PackageService. `created_at` is the ISO 8601
/// timestamp Canton stamps on the create event.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ContractInfo {
    pub contract_id: String,
    pub template_id: String,
    pub package_id: String,
    #[serde(default)]
    pub package_name: String,
    #[serde(default)]
    pub package_version: String,
    #[serde(default)]
    pub created_at: String,
}

/// Vetted package information
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct VettedPackageInfo {
    pub package_id: String,
    pub package_name: String,
    pub package_version: String,
}

/// Package info for peer comparison
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PackageInfo {
    pub package_id: String,
    pub name: String,
    pub version: String,
}

/// Reason a peer was reported `reachable: false` in `PeerPackageResult`.
///
/// Mirrors `NoiseError` variants at a coarser granularity that's stable on
/// the wire — added so the UI / future tooling can distinguish failure
/// modes without having to scan logs. Layered transport-side: TCP connect
/// (timeout/failed), then post-connect request budget (`RequestTimeout`),
/// then mid-stream IO/HTTP (`Transport`); then handshake/decode/status.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "snake_case")]
pub enum PeerErrorKind {
    TcpConnectTimeout,
    TcpConnectFailed,
    RequestTimeout,
    Transport,
    HandshakeFailed,
    BadStatus,
    DecodeFailed,
    InvalidPublicKey,
    Other,
}

/// Result of querying packages from a single peer.
///
/// `error_kind` is `None` when `reachable: true`. Always `Some(_)` when
/// `reachable: false`. (Decode failures on `reachable: true` responses are
/// not yet surfaced — see Future work item 5 in the spec.)
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PeerPackageResult {
    pub participant_id: String,
    pub name: String,
    pub reachable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<PeerErrorKind>,
    #[serde(default)]
    pub packages: Vec<PackageInfo>,
}

/// Response from the peer DAR comparison endpoint
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PeerPackageComparison {
    pub local_packages: Vec<PackageInfo>,
    pub peers: Vec<PeerPackageResult>,
}

/// Party metadata from Ledger API
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PartyMetadata {
    pub annotations: HashMap<String, String>,
}

/// Decentralized party information
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DecentralizedParty {
    pub party_id: CantonId,
    pub threshold: i32,
    pub owners: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_owner_key: Option<String>,
    pub participants: Vec<ParticipantInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contracts: Vec<ContractInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_metadata: Option<PartyMetadata>,
}

/// Connection status for a participant
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "PascalCase")]
pub enum ConnectionStatus {
    /// Current node (always reachable)
    CurrentNode,
    /// Successfully connected via Noise protocol
    Connected,
    /// Failed to establish TCP connection (peer not reachable)
    Unreachable,
    /// Noise handshake/decryption failed (likely wrong public key configured)
    HandshakeFailed,
}

/// Status of a single participant
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ParticipantStatus {
    pub id: String,
    pub status: ConnectionStatus,
    /// Round-trip latency of the health probe, in milliseconds. `None` when the
    /// peer is the current node or was unreachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// The workflow this peer is currently participating in, if any and if it
    /// reported one (peers on older code report `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowInfo>,
    /// dec-party-manager semver: this node's own version for the current
    /// node, or the version a peer reported in its health response. `None` for
    /// unreachable peers and peers on older code that don't report one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Display build identity (image tag / short SHA / `<semver>-dev`) for this
    /// node or the one a peer reported. `None` for unreachable peers and peers
    /// on code that predates the field. This is what the peers table shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_version: Option<String>,
}

/// Response for the participants status endpoint
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ParticipantsStatusResponse {
    pub statuses: Vec<ParticipantStatus>,
}

/// Progress status of a workflow (kick, onboarding, etc.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "lowercase")]
pub enum WorkflowProgress {
    #[default]
    Idle,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

impl WorkflowProgress {
    /// Lowercase label, matching the serde wire representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::InProgress => "inprogress",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for WorkflowProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which workflow this run belongs to. Mirrors InvitationType, but lives on
/// every persisted run (coordinator + peer) regardless of how it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "PascalCase")]
pub enum WorkflowKind {
    Onboarding,
    Kick,
    Contracts,
    Dars,
    AddParty,
    ChangeThreshold,
}

impl WorkflowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Onboarding => "Onboarding",
            Self::Kick => "Kick",
            Self::Contracts => "Contracts",
            Self::Dars => "Dars",
            Self::AddParty => "AddParty",
            Self::ChangeThreshold => "ChangeThreshold",
        }
    }
}

impl std::fmt::Display for WorkflowKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for WorkflowKind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "Onboarding" => Ok(Self::Onboarding),
            "Kick" => Ok(Self::Kick),
            "Contracts" => Ok(Self::Contracts),
            "Dars" => Ok(Self::Dars),
            "AddParty" => Ok(Self::AddParty),
            "ChangeThreshold" => Ok(Self::ChangeThreshold),
            other => Err(anyhow::anyhow!("unknown workflow kind: {other}")),
        }
    }
}

impl From<InvitationType> for WorkflowKind {
    fn from(t: InvitationType) -> Self {
        match t {
            InvitationType::Onboarding => Self::Onboarding,
            InvitationType::Kick => Self::Kick,
            InvitationType::Contracts => Self::Contracts,
            InvitationType::Dars => Self::Dars,
            InvitationType::AddParty => Self::AddParty,
            InvitationType::ChangeThreshold => Self::ChangeThreshold,
        }
    }
}

/// Whether this node is driving the workflow (Coordinator) or signing /
/// participating because it accepted an invite (Peer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "PascalCase")]
pub enum WorkflowRole {
    Coordinator,
    Peer,
}

impl WorkflowRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coordinator => "Coordinator",
            Self::Peer => "Peer",
        }
    }
}

impl std::fmt::Display for WorkflowRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for WorkflowRole {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "Coordinator" => Ok(Self::Coordinator),
            "Peer" => Ok(Self::Peer),
            other => Err(anyhow::anyhow!("unknown workflow role: {other}")),
        }
    }
}

/// `current_step` of the first step of every workflow kind, where the
/// coordinator waits for its invitees to join. Progress on this step is
/// [`WorkflowRun::connected_peers`], not `completed_peers` — nothing completes
/// a step that carries no command.
pub const WAITING_FOR_PEERS_STEP: &str = "WaitingForPeers";

/// Which end of an ACS transfer this node is on.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "lowercase")]
pub enum AcsTransferDirection {
    /// This node is the source, serving blocks out of its export stream.
    Export,
    /// This node is the target, feeding blocks into its Canton import.
    Import,
}

/// How far an offline ACS transfer has got.
///
/// Deliberately carries no total, because there isn't one to carry:
/// `ExportPartyAcsResponse` is a bare `bytes chunk` with no length or count,
/// and counting the party's contracts up front would materialize the whole ACS
/// — the read that has OOM'd nodes. So this drives an indeterminate bar with a
/// throughput readout, never a percentage.
///
/// Sampled rather than continuous. The source reports live session state on
/// every read; the target records a sample every 16 MiB, so `bytes` trails the
/// true figure by up to one sample and `updated_at_ms` is when the counters
/// last moved, not when the run was last polled. A sample that stops advancing
/// is the signal that the transfer has stalled.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcsTransferProgress {
    pub direction: AcsTransferDirection,
    /// Bytes moved so far in this attempt.
    pub bytes: i64,
    /// Sequence number of the most recent block moved.
    pub block: i64,
    /// Unix milliseconds this attempt started. A transfer that breaks restarts
    /// from block 1, so this resets with it and the rate stays honest.
    pub started_at_ms: i64,
    /// Unix milliseconds of this sample, so a stalled transfer is visible as a
    /// timestamp that stops advancing.
    pub updated_at_ms: i64,
}

/// A single persisted workflow run — control-plane state for either the
/// coordinator side or an peer side. The matching artefacts live in
/// `workflow_artifacts` and are looked up by `instance_name`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct WorkflowRun {
    pub instance_name: String,
    pub kind: WorkflowKind,
    pub role: WorkflowRole,
    pub status: WorkflowProgress,
    pub current_step: String,
    pub step_index: i64,
    pub step_total: i64,
    /// JSON-encoded copy of the original *Config struct that started the
    /// workflow — the resume path round-trips it back through serde.
    pub config_json: String,
    /// Hex pubkey of the coordinator. None for coordinator-side rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_pubkey: Option<String>,
    /// The coordinator's own run `instance_name` this peer-side row belongs to
    /// (the invite's `workflow_instance`). Lets instance-scoped CancelInvite /
    /// RetryWorkflow target exactly one of several concurrent runs from the
    /// same coordinator, and gives peer resume its routing key. None for
    /// coordinator-side rows and for rows that predate instance routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_instance: Option<String>,
    /// Resolved coordinator name from the peers table (server-side join,
    /// like get_invitations does for PendingInvitation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_name: Option<String>,
    pub expected_peers: Vec<CantonId>,
    pub completed_peers: Vec<CantonId>,
    /// Peers that have joined this run — the set the `WaitingForPeers` gate
    /// counts, and the only progress a run has to show before its first
    /// peer-gated step. Live in-memory state merged in by the API layer from
    /// the workflow registry (not a DB column), so it is empty for a run this
    /// node does not currently coordinate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connected_peers: Vec<CantonId>,
    /// How far this run's ACS transfer has got, when one is moving. Merged in
    /// by the API layer from the run's artefacts, so it is `None` for every
    /// run and step that shifts no ACS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acs_progress: Option<AcsTransferProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dec_party_id: Option<CantonId>,
    /// Dec party prefix associated with this run (e.g. "UAT"). Populated by
    /// the API layer from `config_json` so the frontend can display a chip
    /// without parsing JSON blobs itself. Not persisted as a column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Participants involved in this run (same source as `prefix`). Empty
    /// when missing from the config payload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<CantonId>,
    /// Kick runs only: the threshold before and after the kick, lifted from
    /// `config_json` so the run card can show "old → new". `None` for every
    /// other workflow kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_threshold: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_threshold: Option<i32>,
    /// Kick runs only: the participant being kicked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kicked_participant: Option<CantonId>,
    /// AddParty runs only: the participant being added. Lifted from
    /// `config_json` by the API layer (not a DB column), same as `prefix`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_participant: Option<CantonId>,
    /// Contracts runs only: package/contract names being deployed. Lifted from
    /// `config_json` by the API layer (not a DB column), same as `prefix`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package_names: Vec<String>,
    /// Dars runs only: DAR filenames being distributed. Lifted from
    /// `config_json` by the API layer (not a DB column).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dar_filenames: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub dismissed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Type of workflow invitation
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "PascalCase")]
pub enum InvitationType {
    Onboarding,
    Kick,
    Contracts,
    Dars,
    AddParty,
    ChangeThreshold,
}

impl InvitationType {
    /// Stable string label used for DB storage. Matches the PascalCase serde repr.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Onboarding => "Onboarding",
            Self::Kick => "Kick",
            Self::Contracts => "Contracts",
            Self::Dars => "Dars",
            Self::AddParty => "AddParty",
            Self::ChangeThreshold => "ChangeThreshold",
        }
    }
}

impl std::fmt::Display for InvitationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for InvitationType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "Onboarding" => Ok(Self::Onboarding),
            "Kick" => Ok(Self::Kick),
            "Contracts" => Ok(Self::Contracts),
            "Dars" => Ok(Self::Dars),
            "AddParty" => Ok(Self::AddParty),
            "ChangeThreshold" => Ok(Self::ChangeThreshold),
            other => Err(anyhow::anyhow!("unknown invitation type: {other}")),
        }
    }
}

/// A pending invitation from a coordinator
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PendingInvitation {
    pub id: String,
    pub invitation_type: InvitationType,
    pub coordinator_pubkey: String,
    #[serde(default)]
    pub coordinator_name: Option<String>,
    pub received_at: i64,
    /// Onboarding-only: party ID prefix the coordinator chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Onboarding-only: full participant list the coordinator selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<CantonId>,
    /// Dars-only: filenames the coordinator is distributing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dar_filenames: Vec<String>,
    /// Dars-only: SHA-256 of each DAR, index-aligned with `dar_filenames`.
    /// Recorded at accept time so the peer can pin the content it agreed to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dar_hashes: Vec<String>,
    /// Kick-only: the participant being removed from the party.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kicked_participant: Option<CantonId>,
    /// Kick-only: threshold after the kick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_threshold: Option<i32>,
    /// Kick-only: threshold before the kick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_threshold: Option<i32>,
    /// Kick-only: dec party the kick targets. Lets the peer card render the
    /// same "Dec party" row the coordinator shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dec_party_id: Option<CantonId>,
    /// AddParty-only: the participant being added to the party.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_participant: Option<CantonId>,
    /// Contracts-only: human-readable package/contract names being deployed,
    /// so the peer card shows the same "Packages" row the coordinator shows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package_names: Vec<String>,
    /// The coordinator's run instance name from the invite payload. Echoed
    /// back on decline so the coordinator only fails the matching run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Frontend authentication configuration response
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(default)]
pub struct AuthConfigResponse {
    /// Whether auth is required (false in test mode or when no provider is configured)
    pub auth_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_realm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth0_domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth0_client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth0_audience: Option<String>,
    /// Extra space-separated scopes the SPA appends to Auth0's default
    /// `openid profile email` request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth0_scope: Option<String>,
}

/// A single governance audit log entry
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AuditLogEntry {
    pub id: i64,
    pub timestamp: i64,
    pub event_type: String,
    pub party_id: CantonId,
    pub member_party_id: CantonId,
    pub governance_type: String,
    pub action_summary: String,
    #[cfg_attr(feature = "typegen", ts(type = "any"))]
    pub details: serde_json::Value,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub created_at: i64,
}

/// The workflow a node is currently participating in, if any.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct WorkflowInfo {
    pub kind: WorkflowKind,
    pub role: WorkflowRole,
    pub step: String,
    pub step_index: i64,
    pub step_total: i64,
}
