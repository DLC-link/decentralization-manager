use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::ParticipantPermission;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};

use crate::{
    config::PackageConfig,
    participant_id::CantonId,
    workflow::contracts::{ContractDefinition, DarFile},
};

/// Trait for workflow status types that can be used with HttpWorkflowState
pub trait WorkflowStatus: Default + Copy + Send + Sync {}

/// Generic state for tracking HTTP-triggered workflows. Holds enough context
/// for the matching `/cancel` endpoint to abort the spawn and notify the
/// peers that received an invite.
pub struct HttpWorkflowState<S: WorkflowStatus> {
    pub status: RwLock<S>,
    pub error: RwLock<Option<String>>,
    pub abort_handle: tokio::sync::Mutex<Option<tokio::task::AbortHandle>>,
    pub invited_peers: RwLock<Vec<CantonId>>,
}

impl<S: WorkflowStatus> Default for HttpWorkflowState<S> {
    fn default() -> Self {
        Self {
            status: RwLock::new(S::default()),
            error: RwLock::new(None),
            abort_handle: tokio::sync::Mutex::new(None),
            invited_peers: RwLock::new(Vec::new()),
        }
    }
}

impl<S: WorkflowStatus> HttpWorkflowState<S> {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Guard that pauses the Noise listener while held and resumes it when dropped.
///
/// `should_pause` is an `AtomicBool` rather than a `bool` behind a lock so that
/// the `Drop` impl can reset it synchronously. The earlier `Arc<RwLock<bool>>`
/// design left the listener stuck in the paused state on task abort or panic
/// (the captured guard was dropped but the async reset never ran), forcing the
/// cancel handler to clean up manually.
pub struct ListenerPauseGuard {
    listener_pause_flag: Arc<AtomicBool>,
    listener_notify: Arc<Notify>,
}

impl ListenerPauseGuard {
    /// Pause the listener and return a guard that will resume it when dropped.
    pub async fn pause(listener_pause_flag: Arc<AtomicBool>, listener_notify: Arc<Notify>) -> Self {
        listener_pause_flag.store(true, Ordering::Release);
        tokio::time::sleep(Duration::from_millis(500)).await;
        Self {
            listener_pause_flag,
            listener_notify,
        }
    }

    /// Resume the listener explicitly. `Drop` does the same thing; calling
    /// this is only useful when you want to ensure the resume has fired
    /// before the surrounding async function returns.
    pub async fn resume(self) {
        // The `Drop` impl below does the actual work; this just consumes
        // `self` so the caller can't keep using the guard.
        drop(self);
    }
}

impl Drop for ListenerPauseGuard {
    fn drop(&mut self) {
        self.listener_pause_flag.store(false, Ordering::Release);
        self.listener_notify.notify_one();
    }
}

/// Cross-workflow mutual exclusion.
///
/// At most one workflow (kick / onboarding / contracts / dars) may run at
/// a time. Each `start_*` handler must `try_acquire` this gate before
/// spawning its coordinator task, then move the returned guard into the
/// spawned task's async block. The guard drops at the end of the task
/// — on success, failure, cancellation, OR panic — so the gate cannot
/// leak past a workflow's lifetime.
///
/// This replaces the previous read-then-write TOCTOU on each per-workflow
/// status `RwLock`: two concurrent `start_kick` calls would both observe
/// `status != InProgress` and both proceed; with this gate, the second
/// `try_acquire` fails and the second caller gets a 409.
pub struct WorkflowInFlightGuard(Arc<AtomicBool>);

impl WorkflowInFlightGuard {
    /// Returns `Some(guard)` if the gate was free and is now held; `None`
    /// if another workflow already holds it.
    pub fn try_acquire(flag: Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(flag))
    }
}

impl Drop for WorkflowInFlightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Participant permission level
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Submission,
    Confirmation,
    Observation,
    Unknown,
}

impl From<i32> for Permission {
    fn from(value: i32) -> Self {
        match value {
            x if x == ParticipantPermission::Submission as i32 => Permission::Submission,
            x if x == ParticipantPermission::Confirmation as i32 => Permission::Confirmation,
            x if x == ParticipantPermission::Observation as i32 => Permission::Observation,
            _ => Permission::Unknown,
        }
    }
}

/// Participant in a decentralized party
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
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
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
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
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct VettedPackageInfo {
    pub package_id: String,
    pub package_name: String,
    pub package_version: String,
}

/// Package info for peer comparison
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, utoipa::ToSchema)]
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
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
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
#[derive(Serialize, utoipa::ToSchema)]
pub struct PeerPackageComparison {
    pub local_packages: Vec<PackageInfo>,
    pub peers: Vec<PeerPackageResult>,
}

/// Party metadata from Ledger API
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PartyMetadata {
    pub annotations: HashMap<String, String>,
}

/// Decentralized party information
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
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

/// Where the response data came from
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ResponseSource {
    /// Fresh data from Canton gRPC
    #[default]
    Live,
    /// Cached data from local database
    Cache,
}

/// Response for the decentralized parties endpoint
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct DecentralizedPartiesResponse {
    pub parties: Vec<DecentralizedParty>,
    #[serde(default)]
    pub source: ResponseSource,
    /// Whether a background refresh from Canton is currently in progress
    #[serde(default)]
    pub refreshing: bool,
}

/// Connection status for a participant
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, utoipa::ToSchema)]
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
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct ParticipantStatus {
    pub id: String,
    pub status: ConnectionStatus,
}

/// Response for the participants status endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct ParticipantsStatusResponse {
    pub statuses: Vec<ParticipantStatus>,
}

/// Request to kick a participant from a decentralized party.
/// `deny_unknown_fields` rejects pre-fix requests carrying
/// `namespace_fingerprint` instead of silently ignoring it (the server now
/// derives it from cache; see `start_kick`).
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KickRequest {
    pub decentralized_party_id: CantonId,
    pub participant_id: CantonId,
    pub new_threshold: i32,
}

/// Request to create a new decentralized party
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct OnboardingRequest {
    /// Party ID prefix for the decentralized party (e.g., "xyz-network")
    pub party_id_prefix: String,
    /// List of peer IDs to invite to the decentralized party
    pub peer_ids: Vec<CantonId>,
}

/// Why a directed edge was reported missing. The frontend renders different
/// remediation hints depending on which kind it sees: `MeshHole` is a true
/// peer↔peer config gap ("on `from`, add `to` to the network config"), while
/// `UnreachableFromCoordinator` is a coordinator-side reachability problem
/// (the peer is unknown, has no public key, didn't answer, or replied with
/// a malformed payload — fix the coordinator's view of `to`, or `to` itself).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MissingEdgeKind {
    UnreachableFromCoordinator,
    MeshHole,
}

/// One directed missing edge in the peer mesh: `from` does not have `to`
/// configured as a peer (`MeshHole`), or the coordinator could not query
/// `to` at all (`UnreachableFromCoordinator`, `from` is the coordinator).
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct MissingPeerEdge {
    pub from: String,
    pub to: String,
    pub kind: MissingEdgeKind,
}

/// Returned when onboarding pre-flight detects that selected peers are not
/// fully meshed. The workflow is not started.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct OnboardingMeshErrorResponse {
    pub error: String,
    pub missing_edges: Vec<MissingPeerEdge>,
}

/// Request to deploy contracts for a decentralized party
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct ContractsRequest {
    /// Decentralized party ID to deploy contracts for
    pub decentralized_party_id: CantonId,
    /// List of participant IDs that will sign submissions
    pub participant_ids: Vec<CantonId>,
    /// List of party IDs for each participant (must match participant_ids order)
    pub participant_parties: Vec<CantonId>,
    /// Operator party ID
    pub operator_party: CantonId,
    /// Contract definitions to create after decentralized party setup
    #[serde(default)]
    pub contracts: Vec<ContractDefinition>,
}

/// Request to upload DARs across all participants
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct DarsRequest {
    /// DAR files to upload (base64-encoded)
    pub dar_files: Vec<DarFile>,
    /// Peer IDs to distribute to (required non-empty for /dars/distribute, ignored by /dars/upload)
    #[serde(default)]
    pub peer_ids: Vec<CantonId>,
}

/// Progress status of a workflow (kick, onboarding, etc.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowProgress {
    #[default]
    Idle,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

impl WorkflowStatus for WorkflowProgress {}

/// Type aliases for backwards compatibility
pub type KickStatus = WorkflowProgress;
pub type OnboardingStatus = WorkflowProgress;

/// Response for workflow initiation (kick, onboarding, etc.)
#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkflowResponse {
    pub status: WorkflowProgress,
    pub message: String,
}

/// Type aliases for backwards compatibility
pub type KickResponse = WorkflowResponse;
pub type OnboardingResponse = WorkflowResponse;

/// Which workflow this run belongs to. Mirrors InvitationType, but lives on
/// every persisted run (coordinator + peer) regardless of how it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "PascalCase")]
pub enum WorkflowKind {
    Onboarding,
    Kick,
    Contracts,
    Dars,
}

impl WorkflowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Onboarding => "Onboarding",
            Self::Kick => "Kick",
            Self::Contracts => "Contracts",
            Self::Dars => "Dars",
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
        }
    }
}

/// Whether this node is driving the workflow (Coordinator) or signing /
/// participating because it accepted an invite (Peer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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

/// A single persisted workflow run — control-plane state for either the
/// coordinator side or an peer side. The matching artefacts live in
/// `workflow_artifacts` and are looked up by `instance_name`.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinator_pubkey: Option<String>,
    /// Resolved coordinator name from the peers table (server-side join,
    /// like get_invitations does for PendingInvitation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinator_name: Option<String>,
    pub expected_peers: Vec<CantonId>,
    pub completed_peers: Vec<CantonId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dec_party_id: Option<CantonId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub dismissed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Response wrapper for `GET /workflows`.
#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkflowRunsResponse {
    pub runs: Vec<WorkflowRun>,
}

/// Payload for the `CancelWorkflow` Noise message — the coordinator tells an
/// peer to abort its in-flight run for `instance_name`.
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CancelWorkflowPayload {
    pub instance_name: String,
}

/// Response for key status check
#[derive(Serialize, utoipa::ToSchema)]
pub struct KeyStatusResponse {
    pub has_keys: bool,
    pub public_key: Option<String>,
}

/// Type of workflow invitation
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "PascalCase")]
pub enum InvitationType {
    Onboarding,
    Kick,
    Contracts,
    Dars,
}

impl InvitationType {
    /// Stable string label used for DB storage. Matches the PascalCase serde repr.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Onboarding => "Onboarding",
            Self::Kick => "Kick",
            Self::Contracts => "Contracts",
            Self::Dars => "Dars",
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
            other => Err(anyhow::anyhow!("unknown invitation type: {other}")),
        }
    }
}

/// Payload sent inside an `InviteOnboarding` Noise message.
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct OnboardingInvitePayload {
    pub prefix: String,
    pub participants: Vec<CantonId>,
}

/// Payload sent inside an `InviteDars` Noise message.
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DarsInvitePayload {
    pub dar_filenames: Vec<String>,
}

/// A pending invitation from a coordinator
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct PendingInvitation {
    pub id: String,
    pub invitation_type: InvitationType,
    pub coordinator_pubkey: String,
    pub coordinator_name: Option<String>,
    pub received_at: i64,
    /// Onboarding-only: party ID prefix the coordinator chose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Onboarding-only: full participant list the coordinator selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<CantonId>,
    /// Dars-only: filenames the coordinator is distributing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dar_filenames: Vec<String>,
}

/// Response for pending invitations endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct PendingInvitationsResponse {
    pub invitations: Vec<PendingInvitation>,
}

/// Request to accept or decline an invitation
#[derive(Deserialize, utoipa::ToSchema)]
pub struct InvitationActionRequest {
    pub id: String,
}

/// Authentication status for a party
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AuthStatus {
    Authenticated,
    Mock,
    Failed { error: String },
    NotConfigured,
}

/// User rights validation result
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct RightsStatus {
    /// Whether user can actAs the member party
    pub member_party_act_as: bool,
    /// Whether user can readAs the member party
    pub member_party_read_as: bool,
    /// Whether user can actAs the decentralized party
    pub dec_party_act_as: bool,
    /// Whether user can readAs the decentralized party
    pub dec_party_read_as: bool,
}

impl RightsStatus {
    /// Check if all required rights are present
    pub fn is_valid(&self) -> bool {
        self.member_party_act_as
            && self.member_party_read_as
            && self.dec_party_act_as
            && self.dec_party_read_as
    }
}

/// Authentication status for a single party
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct PartyAuthStatus {
    pub dec_party_id: CantonId,
    pub member_party_id: CantonId,
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_realm: Option<String>,
    pub status: AuthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights: Option<RightsStatus>,
}

/// Response for the auth status endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct AuthStatusResponse {
    pub parties: Vec<PartyAuthStatus>,
}

/// Result of an authentication test
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct AuthTestResult {
    pub party_id: CantonId,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response for the auth test endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct AuthTestResponse {
    pub results: Vec<AuthTestResult>,
}

/// One participant's member party for a given dec party. `None`
/// member_party_id = peer not configured / unreachable.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct KnownMember {
    pub participant_uid: CantonId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_party_id: Option<CantonId>,
}

/// Response for `GET /governance/known-members`.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct KnownMembersResponse {
    pub members: Vec<KnownMember>,
}

/// Request body for `POST /party-config/discover-member-party`. Same Keycloak
/// shape as `PartyConfigRequest`, used to mint a one-shot token and look up
/// the authenticated user's primary party from Canton's UserManagementService.
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct DiscoverMemberPartyRequest {
    pub keycloak_url: String,
    pub keycloak_realm: String,
    pub keycloak_client_id: String,
    #[serde(default)]
    pub keycloak_client_secret: Option<String>,
    #[serde(default)]
    pub keycloak_username: Option<String>,
    #[serde(default)]
    pub keycloak_password: Option<String>,
}

/// Response for `POST /party-config/discover-member-party`. `primary_party`
/// is `None` when Canton's user has no primary party assigned.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct DiscoverMemberPartyResponse {
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_party: Option<CantonId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Request to grant the configured user the rights they need to act on a dec party
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct GrantRightsRequest {
    /// Decentralized party whose coordinator user should receive rights
    pub dec_party_id: CantonId,
    /// Keycloak client_id of the admin (validator) client whose service-account
    /// user has ParticipantAdmin on Canton. Provided per-call by the operator;
    /// never stored.
    pub admin_client_id: String,
    /// Keycloak client_secret matching admin_client_id. Provided per-call by
    /// the operator; never stored.
    pub admin_client_secret: String,
}

/// Response for the grant-rights endpoint
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct GrantRightsResponse {
    /// Refreshed rights status after the grant call
    pub rights: RightsStatus,
}

// ============================================================================
// Governance Types (Structured Actions)
// ============================================================================

/// Instrument identifier (admin + id)
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct InstrumentId {
    pub admin: String,
    pub id: String,
}

/// Vault limits configuration (all fields are optional in DAML)
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct VaultLimits {
    #[schema(value_type = Option<String>)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_total_deposit: Option<DamlDecimal>,
    #[schema(value_type = Option<String>)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_deposit_amount: Option<DamlDecimal>,
    #[schema(value_type = Option<String>)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_withdrawal_amount: Option<DamlDecimal>,
}

/// Featured App Right beneficiary
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AppRewardBeneficiary {
    pub beneficiary: CantonId,
    #[schema(value_type = String)]
    pub weight: DamlDecimal,
}

/// Featured App Right configuration
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FarConfig {
    pub featured_app_right_cid: String,
    pub beneficiaries: Vec<AppRewardBeneficiary>,
}

/// Structured action types for Vault governance
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionType {
    // Governance (4)
    GovernanceAddMember {
        member: CantonId,
        new_threshold: i64,
    },
    GovernanceRemoveMember {
        member: CantonId,
        new_threshold: i64,
    },
    GovernanceSetThreshold {
        new_threshold: i64,
    },
    GovernanceSetTimeout {
        new_timeout_microseconds: i64,
    },
    GovernanceAddAdditionalProposer {
        additional_proposer: CantonId,
    },
    GovernanceRemoveAdditionalProposer {
        additional_proposer: CantonId,
    },

    // Vault Deployment (2)
    VaultDeployment {
        vault_rules_cid: String,
        vault_name: String,
        share_symbol: String,
        asset_instrument_id: InstrumentId,
        limits: VaultLimits,
        vault_backend_signatory: CantonId,
        #[serde(default)]
        vault_far_config: Option<FarConfig>,
        allocation_factory_cid: String,
        registrar_service_cid: String,
    },
    YieldEpochDeployment {
        vault_rules_cid: String,
        vault_cid: String,
        asset_instrument_id: InstrumentId,
        vault_backend_signatory: CantonId,
    },

    // Vault Operations (5)
    VaultPause {
        vault_id: String,
    },
    VaultUnpause {
        vault_id: String,
    },
    VaultUpdateLimits {
        vault_id: String,
        new_limits: VaultLimits,
    },
    VaultUpdateBackend {
        vault_id: String,
        new_backend_signatory: CantonId,
    },
    VaultUpdateFarBeneficiaries {
        vault_id: String,
        new_beneficiaries: Vec<AppRewardBeneficiary>,
    },

    // Processor (1)
    ProcessorDeploymentRequest {
        vault_processor_rules_cid: String,
        vault_backend_signatory: CantonId,
        allocation_factory_cid: String,
        #[serde(default)]
        processor_far_config: Option<FarConfig>,
        initial_supported_vaults: Vec<String>,
    },

    // Utility Onboarding (4)
    UtilityCreateProviderRequest {
        operator: CantonId,
    },
    UtilityCreateUserRequest {
        operator: CantonId,
    },
    UtilitySetup {
        operator: CantonId,
        provider_service_cid: String,
        user_service_cid: String,
    },
    UtilityAcceptHolderServiceRequest {
        operator: CantonId,
        provider_service_cid: String,
        holder_service_request_cid: String,
        holder: CantonId,
    },
    // Credential Actions (2)
    CredentialOfferFree {
        operator: CantonId,
        user_service_cid: String,
        holder: CantonId,
        id: String,
        description: String,
        claims: Vec<Claim>,
    },
    CredentialAcceptFree {
        operator: CantonId,
        user_service_cid: String,
        credential_offer_cid: String,
    },

    // DevNet (1)
    DevNetFeatureApp {
        amulet_rules_cid: String,
    },
}

impl ActionType {
    /// Validate the action's fields. Returns an error message if invalid.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            ActionType::VaultDeployment {
                vault_far_config: Some(far),
                ..
            }
            | ActionType::ProcessorDeploymentRequest {
                processor_far_config: Some(far),
                ..
            } => validate_beneficiary_weights(&far.beneficiaries),
            ActionType::VaultUpdateFarBeneficiaries {
                new_beneficiaries, ..
            } => validate_beneficiary_weights(new_beneficiaries),
            _ => Ok(()),
        }
    }
}

fn validate_beneficiary_weights(beneficiaries: &[AppRewardBeneficiary]) -> Result<(), String> {
    if beneficiaries.is_empty() {
        return Ok(());
    }
    let sum: DamlDecimal = beneficiaries.iter().map(|b| b.weight).sum();
    let one: DamlDecimal = "1".parse().expect("'1' is a valid DamlDecimal");
    if sum != one {
        return Err(format!(
            "FAR beneficiary weights must sum to exactly 1.0, got {sum}"
        ));
    }
    Ok(())
}

/// Credential claim (subject, property, value)
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Claim {
    pub subject: String,
    pub property: String,
    pub value: String,
}

/// Billing parameters for a paid credential.
/// Mirrors `Utility.Credential.App.V0.Types.BillingParams`.
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BillingParams {
    /// The daily fee for the credential in USD (corresponds to RatePerDay record).
    #[schema(value_type = String)]
    pub fee_per_day_usd: DamlDecimal,
    /// Duration between fee charges, in minutes.
    pub billing_period_minutes: i64,
    /// Target deposit amount in USD.
    #[schema(value_type = String)]
    pub deposit_target_amount_usd: DamlDecimal,
    /// Holder's weight on the activity marker (0.0 - 1.0). None means 0.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub holder_activity_weight: Option<DamlDecimal>,
}

/// Which governance system a request targets
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceType {
    /// VaultGovernanceRules (closed-enum inline actions)
    #[default]
    Vault,
    /// GovernanceRules self-management (GovernanceSelfAction)
    CoreSelf,
    /// GovernanceRules domain actions (GovernableAction proposals)
    CoreDomain,
}

/// Instrument allowance for token preapproval
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct InstrumentAllowance {
    pub id: String,
}

/// Types of governance domain action proposals
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProposalType {
    /// Set up Canton Coin TransferPreapproval
    SetupCcPreapproval {
        provider: CantonId,
        expected_dso: CantonId,
    },
    /// Set up utility token TransferPreapproval
    SetupTokenPreapproval {
        operator: CantonId,
        instrument_admin: CantonId,
        #[serde(default)]
        instrument_allowances: Vec<InstrumentAllowance>,
    },
    /// Transfer tokens via a TransferFactory
    Transfer {
        transfer_factory_cid: String,
        expected_admin: CantonId,
        receiver: CantonId,
        #[schema(value_type = String)]
        amount: DamlDecimal,
        instrument_id: InstrumentId,
        #[serde(default)]
        input_holding_cids: Vec<String>,
    },
    /// Accept an incoming token transfer
    AcceptTransfer { transfer_instruction_cid: String },
    /// Generic text-based vote (no on-chain effect beyond recording the result)
    GenericVote { description: String },
    /// Provision a Utility-Registry `ProviderService` with
    /// `operator = proposer` and `provider = governanceParty`. Produces the
    /// ProviderService cid consumed by `SetupUtility`.
    ProvisionProviderService,
    /// Run the full Utility-Registry onboarding in one vote. Flags control
    /// whether a `TransferRule` / `AllocationFactory` are created during the
    /// `RegistrarServiceRequest` accept.
    SetupUtility {
        provider_service_cid: String,
        operator: CantonId,
        instrument_id_text: String,
        create_transfer_rule: bool,
        create_allocation_factory: bool,
    },
    /// Create a `ProviderServiceRequest` for a given `operator` and `provider`.
    CreateProviderServiceRequest {
        operator: CantonId,
        provider: CantonId,
    },
    /// Create a `UserServiceRequest` for a given `operator` and `user`.
    CreateUserServiceRequest { operator: CantonId, user: CantonId },
    /// Set the provider-app reward beneficiaries on an `InstrumentConfiguration`.
    /// `providerAppRewardBeneficiaries = None` clears the current setting.
    SetProviderAppRewardBeneficiaries {
        instrument_configuration_cid: String,
        #[serde(default)]
        provider_app_reward_beneficiaries: Option<Vec<AppRewardBeneficiary>>,
    },
    /// Toggle result-contract emission on a `RegistrarService`.
    SetEnableResultContracts {
        registrar_service_cid: String,
        #[serde(default)]
        enable_result_contracts: Option<bool>,
    },
    /// Authorize the `operator` to create batched activity markers on behalf
    /// of the governance party via a `DelegatedBatchedMarkersProxy`.
    CreateDelegatedBatchedMarkersProxy { operator: CantonId },
    /// Offer a mint of `amount` tokens to `recipient` via
    /// `AllocationFactory_OfferMint`. The resulting `MintOffer` is accepted
    /// later by the recipient, outside this plugin.
    Mint {
        allocation_factory_cid: String,
        instrument_id: InstrumentId,
        instrument_configuration_cid: String,
        recipient: CantonId,
        #[schema(value_type = String)]
        amount: DamlDecimal,
        description: String,
    },
    /// Offer a free credential to a holder via the governance party's
    /// `UserService`. Wraps `UserService_OfferFreeCredential` from the
    /// Utility Credential App.
    OfferFreeCredential {
        user_service_cid: String,
        holder: CantonId,
        id: String,
        description: String,
        claims: Vec<Claim>,
    },
    /// Offer a paid credential to a holder via the governance party's
    /// `UserService`. Wraps `UserService_OfferPaidCredential`.
    OfferPaidCredential {
        user_service_cid: String,
        holder: CantonId,
        id: String,
        description: String,
        claims: Vec<Claim>,
        billing_params: BillingParams,
        #[serde(default)]
        #[schema(value_type = Option<String>)]
        deposit_initial_amount_usd: Option<DamlDecimal>,
    },
    /// Accept a free credential offered to the governance party. Wraps
    /// `UserService_AcceptFreeCredentialOffer`.
    AcceptFreeCredential {
        user_service_cid: String,
        credential_offer_cid: String,
    },
    /// Offer a burn of `amount` tokens held by `holder` via
    /// `AllocationFactory_OfferBurn`. Holdings are supplied by the holder at
    /// `BurnOffer_Accept` time, not here.
    Burn {
        allocation_factory_cid: String,
        instrument_id: InstrumentId,
        instrument_configuration_cid: String,
        holder: CantonId,
        #[schema(value_type = String)]
        amount: DamlDecimal,
        description: String,
    },
    /// Accept a holder-initiated `MintRequest` via `MintRequest_Accept`. The
    /// `MintRequest` must already exist on-ledger (typically created by the
    /// holder by exercising `AllocationFactory_RequestMint`).
    AcceptMintRequest {
        mint_request_cid: String,
        instrument_configuration_cid: String,
        description: String,
    },
    /// Accept a holder-initiated `BurnRequest` via `BurnRequest_Accept`. The
    /// `BurnRequest` must already exist on-ledger (typically created by the
    /// holder by exercising `AllocationFactory_RequestBurn`).
    AcceptBurnRequest {
        burn_request_cid: String,
        instrument_configuration_cid: String,
        description: String,
    },
}

/// Request to propose a governance domain action (creates proposal contract)
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ProposeActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub proposal: ProposalType,
}

/// A pending domain action proposal with its confirmations
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct DomainGovernanceAction {
    /// Contract ID of the proposal
    pub proposal_cid: String,
    /// Human-readable label (e.g., "SetupCcPreapproval")
    pub action_label: String,
    /// Human-readable description from the proposal's GovernableActionView
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Confirmations for this proposal
    pub confirmations: Vec<GovernanceConfirmation>,
    /// Number of unique confirmers
    pub confirmation_count: usize,
    /// Whether threshold is met for execution
    pub can_execute: bool,
    /// `true` when the underlying proposal contract was not found in this
    /// participant's ACS at query time. Confirmations referencing an archived
    /// proposal can't be confirmed/executed (the proposal cid is gone), but
    /// the Confirmation contracts themselves are still active and need to be
    /// expired explicitly to clear them off the ledger. The UI uses this
    /// flag to render a dismiss-only card instead of the normal Confirm /
    /// Execute affordances.
    #[serde(default)]
    pub orphaned: bool,
}

/// Request to submit a confirmation for an action with structured type
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ConfirmActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub action: ActionType,
    #[serde(default)]
    pub governance_type: GovernanceType,
    /// For CoreDomain: ContractId of the GovernableAction proposal
    #[serde(default)]
    pub proposal_cid: Option<String>,
}

/// A disclosed contract to include in the ledger submission
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct DisclosedContractInput {
    pub contract_id: String,
    pub blob: String, // base64-encoded created_event_blob
}

/// Request to execute a confirmed action with structured type
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ExecuteActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub action: ActionType,
    pub confirmation_cids: Vec<String>,
    #[serde(default)]
    pub disclosed_contracts: Vec<DisclosedContractInput>,
    #[serde(default)]
    pub governance_type: GovernanceType,
    /// For CoreDomain: ContractId of the GovernableAction proposal
    #[serde(default)]
    pub proposal_cid: Option<String>,
}

/// A single governance confirmation with parsed action
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct GovernanceConfirmation {
    pub contract_id: String,
    pub action: ActionType,
    pub confirming_party: CantonId,
    /// Unix seconds when the confirmation contract was created on the ledger.
    /// 0 if the timestamp could not be resolved.
    #[serde(default)]
    pub created_at: i64,
}

/// A governance action with its confirmations, grouped by action hash
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct GovernanceAction {
    /// Deterministic hash of the serialized action for grouping
    pub action_hash: String,
    /// The parsed action type
    pub action: ActionType,
    /// List of confirmations for this action
    pub confirmations: Vec<GovernanceConfirmation>,
    /// Number of confirmations
    pub confirmation_count: usize,
    /// Whether threshold is met for execution
    pub can_execute: bool,
    /// Unix seconds of the most recent confirmation (used for sorting in UI).
    #[serde(default)]
    pub last_confirmation_at: i64,
}

/// Response for governance confirmations endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct GovernanceResponse {
    pub actions: Vec<GovernanceAction>,
    /// Pending domain action proposals (governance-core GovernableAction)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain_actions: Vec<DomainGovernanceAction>,
    pub threshold: usize,
    /// The member party ID for the requesting party (used to identify own confirmations)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_party_id: Option<CantonId>,
    /// Current contract id of the active GovernanceRules / VaultGovernanceRules
    /// contract for this party. The choice exercised when confirming an action
    /// is consuming, so this id changes after each confirm/execute — clients
    /// should use this field rather than a cached value to avoid
    /// `CONTRACT_NOT_FOUND` on stale ids.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules_contract_id: Option<String>,
}

/// Request to expire a stale confirmation
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ExpireConfirmationRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub confirmation_cid: String,
    #[serde(default)]
    pub governance_type: GovernanceType,
}

/// Request to cancel (revoke) own confirmation
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct CancelConfirmationRequest {
    pub party_id: CantonId,
    pub confirmation_cid: String,
    #[serde(default)]
    pub governance_type: GovernanceType,
}

/// State of a VaultGovernanceRules contract
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct GovernanceState {
    pub contract_id: String,
    pub vault_manager: CantonId,
    pub members: Vec<CantonId>,
    pub threshold: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_confirmation_timeout_microseconds: Option<i64>,
}

/// Response for the governance state endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct GovernanceStateResponse {
    pub state: Option<GovernanceState>,
}

/// Information about a deployed Vault contract
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct VaultInfo {
    pub contract_id: String,
    pub vault_name: String,
    pub share_symbol: String,
    pub is_paused: bool,
    pub vault_manager: CantonId,
}

/// Response for the vaults endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct VaultsResponse {
    pub vaults: Vec<VaultInfo>,
}

/// Information about a ProviderService contract
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct ProviderServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub provider: CantonId,
}

/// Response for the provider services endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct ProviderServicesResponse {
    pub services: Vec<ProviderServiceInfo>,
}

/// Information about a UserService contract
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct UserServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub user: CantonId,
}

/// Information about an InstrumentConfiguration contract (one "token" the
/// governance party can mint/burn against). `instrument_admin` and
/// `instrument_id` are read off the contract's `defaultIdentifier` field and
/// match the `InstrumentId { admin, id }` shape required by Mint/Burn
/// proposals.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct InstrumentInfo {
    pub contract_id: String,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
}

/// Response for the instruments endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct InstrumentsResponse {
    pub instruments: Vec<InstrumentInfo>,
}

/// Response for the user services endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct UserServicesResponse {
    pub services: Vec<UserServiceInfo>,
}

/// Information about a RegistrarService contract
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct RegistrarServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub registrar: CantonId,
}

/// Response for the registrar services endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct RegistrarServicesResponse {
    pub services: Vec<RegistrarServiceInfo>,
}

/// A contract ID with its blob
#[derive(Serialize, utoipa::ToSchema)]
pub struct ContractWithBlob {
    pub contract_id: String,
    pub blob: String,
}

/// DSO network info (amulet rules + DSO party)
#[derive(Serialize, utoipa::ToSchema)]
pub struct NetworkInfo {
    pub dso_party_id: CantonId,
    pub amulet_rules_cid: String,
    pub amulet_rules_blob: String,
}

/// DA Utility operator info (operator party id)
#[derive(Serialize, utoipa::ToSchema)]
pub struct OperatorInfo {
    pub party_id: CantonId,
}

/// Count of active `TransferPreapproval` contracts a governance party already
/// has, split by direction. The UI uses this to warn the user that re-issuing
/// a `SetupCcPreapproval` / `SetupTokenPreapproval` proposal is pointless
/// (the on-chain choice would fail when executed).
#[derive(Serialize, utoipa::ToSchema)]
pub struct TransferPreapprovalsResponse {
    /// `Splice.Wallet.TransferPreapproval:TransferPreapproval` — Canton Coin
    pub cc: usize,
    /// `Utility.Registry.App.V0.Model.TransferPreapproval:TransferPreapproval`
    pub token: usize,
}

/// Response for the generic contract query endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct ContractQueryResponse {
    pub contracts: Vec<ContractWithBlob>,
}

/// Request to save or update party configuration
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct PartyConfigRequest {
    /// The decentralized party ID
    pub dec_party_id: CantonId,
    /// The member party ID (local to this node)
    pub member_party_id: CantonId,
    /// Canton/Ledger API user ID
    pub user_id: String,
    /// Keycloak server URL
    pub keycloak_url: String,
    /// Keycloak realm name
    pub keycloak_realm: String,
    /// OAuth2 client ID
    pub keycloak_client_id: String,
    /// Client secret for M2M flow (None = keep existing, "" = clear)
    #[serde(default)]
    pub keycloak_client_secret: Option<String>,
    /// Username for password flow (None = keep existing, "" = clear)
    #[serde(default)]
    pub keycloak_username: Option<String>,
    /// Password for password flow (None = keep existing, "" = clear)
    #[serde(default)]
    pub keycloak_password: Option<String>,
    /// Package identifiers for deployed Daml contracts
    #[serde(default)]
    pub packages: PackageConfig,
}

/// Response with party configuration (secrets masked)
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct PartyConfigResponse {
    /// The decentralized party ID
    pub dec_party_id: CantonId,
    /// `None` when no credentials are saved yet — operator must provide one via PUT.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_party_id: Option<CantonId>,
    /// `None` when no credentials are saved yet — operator picks via Discover or types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Keycloak server URL
    pub keycloak_url: String,
    /// Keycloak realm name
    pub keycloak_realm: String,
    /// OAuth2 client ID
    pub keycloak_client_id: String,
    /// Whether a client secret is configured
    pub has_client_secret: bool,
    /// Whether a username is configured
    pub has_username: bool,
    /// Whether a password is configured
    pub has_password: bool,
    /// Package identifiers for deployed Daml contracts
    pub packages: PackageConfig,
}

/// Frontend authentication configuration response
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct AuthConfigResponse {
    /// Whether Keycloak auth is required (false in test mode)
    pub auth_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_realm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_client_id: Option<String>,
}

/// Generic error response
#[derive(Serialize, utoipa::ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

/// Generic success response
#[derive(Serialize, utoipa::ToSchema)]
pub struct MessageResponse {
    pub message: String,
}

/// Generic success boolean response
#[derive(Serialize, utoipa::ToSchema)]
pub struct SuccessResponse {
    pub success: bool,
}

/// Response for workflow status check endpoints
#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkflowStatusResponse {
    pub status: WorkflowProgress,
    pub error: Option<String>,
}

// ============================================================================
// Audit Trail Types
// ============================================================================

/// Query parameters for the governance audit endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AuditLogQuery {
    /// Decentralized party ID to filter audit entries
    pub party_id: CantonId,
    /// Maximum number of entries to return (default 50)
    #[serde(default = "default_audit_limit")]
    pub limit: i64,
    /// Offset for pagination (default 0)
    #[serde(default)]
    pub offset: i64,
}

fn default_audit_limit() -> i64 {
    50
}

/// A single governance audit log entry
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct AuditLogEntry {
    pub id: i64,
    pub timestamp: i64,
    pub event_type: String,
    pub party_id: CantonId,
    pub member_party_id: CantonId,
    pub governance_type: String,
    pub action_summary: String,
    pub details: serde_json::Value,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub created_at: i64,
}

/// Response for the governance audit endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct AuditLogResponse {
    pub entries: Vec<AuditLogEntry>,
    pub total_returned: usize,
}

// ============================================================================
// Chain Audit Trail Types
// ============================================================================

/// Query parameters for the on-chain governance audit endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ChainAuditQuery {
    /// Decentralized party ID to query chain events for
    pub party_id: CantonId,
    /// Maximum number of entries to return (default 100)
    #[serde(default = "default_chain_audit_limit")]
    pub limit: usize,
    /// When true, fetches fresh data from Canton and updates cache
    #[serde(default)]
    pub refresh: bool,
}

fn default_chain_audit_limit() -> usize {
    100
}

/// A single on-chain governance audit entry
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct ChainAuditEntry {
    /// Ledger offset (used for sort/pagination)
    pub offset: i64,
    /// Transaction effective_at in epoch seconds
    pub timestamp: i64,
    /// propose | confirm | execute | expire | cancel | create | other
    pub event_type: String,
    pub contract_id: String,
    /// "Module:Entity"
    pub template_id: String,
    pub package_id: String,
    /// vault | core_self | core_domain | cbtc | unknown
    pub governance_type: String,
    pub action_summary: String,
    /// Exercised choice name (None for Created events)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    /// Signatories (Created) or actingParties (Exercised)
    pub acting_parties: Vec<String>,
    pub update_id: String,
    /// Create arguments or choice argument as JSON
    pub details: serde_json::Value,
}

impl From<crate::db::rows::ChainAuditCacheRow> for ChainAuditEntry {
    fn from(row: crate::db::rows::ChainAuditCacheRow) -> Self {
        Self {
            offset: row.offset,
            timestamp: row.timestamp,
            event_type: row.event_type,
            contract_id: row.contract_id,
            template_id: row.template_id,
            package_id: row.package_id,
            governance_type: row.governance_type,
            action_summary: row.action_summary,
            choice: row.choice,
            acting_parties: serde_json::from_str(&row.acting_parties).unwrap_or_default(),
            update_id: row.update_id,
            details: serde_json::from_str(&row.details).unwrap_or(serde_json::Value::Null),
        }
    }
}

/// Response for the on-chain governance audit endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct ChainAuditResponse {
    pub entries: Vec<ChainAuditEntry>,
    pub total_returned: usize,
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    /// P3: locks the wire shape of `WorkflowRun` so the `String → CantonId`
    /// typing change for participant-id fields cannot silently switch from
    /// plain strings to nested objects on the JSON the frontend consumes.
    #[test]
    fn workflow_run_serializes_canton_ids_as_plain_strings() {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let participant_id_str = format!("participant::{ns}");
        let dec_party_id_str = format!("test-network-1::{ns}");

        let peer_a = CantonId::parse(&format!("participant::{ns}")).unwrap();
        let peer_b = CantonId::parse(&format!(
            "participant::1220{0}{0}",
            "abcdefabcdefabcdefabcdefabcdef00"
        ))
        .unwrap();

        let run = WorkflowRun {
            instance_name: "test-network-1-creation".to_string(),
            kind: WorkflowKind::Onboarding,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: "WaitingForPeers".to_string(),
            step_index: 0,
            step_total: 7,
            config_json: r#"{"prefix":"test-network-1"}"#.to_string(),
            coordinator_pubkey: None,
            coordinator_name: None,
            expected_peers: vec![peer_a.clone(), peer_b.clone()],
            completed_peers: vec![peer_a],
            dec_party_id: Some(CantonId::parse(&dec_party_id_str).unwrap()),
            error: None,
            dismissed: false,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_001,
        };

        let json = serde_json::to_value(&run).expect("serialize WorkflowRun");

        // expected_peers and completed_peers must be JSON arrays of
        // plain strings — never objects with prefix/namespace fields.
        let expected = json
            .get("expected_peers")
            .and_then(Value::as_array)
            .expect("expected_peers must be a JSON array");
        assert_eq!(expected.len(), 2);
        for v in expected {
            assert!(
                v.is_string(),
                "expected_peers entry must be a string, got {v}"
            );
        }
        assert_eq!(expected[0].as_str().unwrap(), participant_id_str);

        let completed = json
            .get("completed_peers")
            .and_then(Value::as_array)
            .expect("completed_peers must be a JSON array");
        assert_eq!(completed.len(), 1);
        assert!(completed[0].is_string());

        // dec_party_id (Option<CantonId>) must serialize as a plain string,
        // not as a nested object with prefix/namespace fields.
        let dec_party = json.get("dec_party_id").expect("dec_party_id key present");
        assert!(
            dec_party.is_string(),
            "dec_party_id must be a JSON string when set, got {dec_party}"
        );
        assert_eq!(dec_party.as_str().unwrap(), dec_party_id_str);
    }
}
