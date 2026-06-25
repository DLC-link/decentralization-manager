//! HTTP wire DTOs for the `decman` server's REST API, shared with the
//! `decman-cli` client and used to generate the frontend's TypeScript types
//! (see the `gen-types` binary, which derives them from this module via ts-rs).
//!
//! Like [`crate::types`], these are pure data-transfer types: no server-only
//! dependencies. The OpenAPI (`utoipa`) derives are gated behind the `openapi`
//! feature so dependency-light clients don't inherit them.
//!
//! Types that carry on-chain decimal amounts (`DamlDecimal`) or feed Canton
//! ledger-command construction (e.g. `ActionType`, `ProposalType`) stay in the
//! `decman` crate for now — moving them would pull the heavy `canton_common`
//! dependency into this lightweight crate.

use serde::{Deserialize, Serialize};

use crate::{
    canton_id::CantonId,
    types::{
        AuditLogEntry, DecentralizedParty, PendingInvitation, WorkflowKind, WorkflowProgress,
        WorkflowRun,
    },
};

// ============================================================================
// Config DTOs (shared with the server's config layer)
// ============================================================================

/// Package identifiers for Daml contracts (configurable per party)
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PackageConfig {
    pub governance_action: Option<String>,
    pub governance_core: Option<String>,
    pub governance_token_custody: Option<String>,
    pub governance_utility_credential: Option<String>,
    pub governance_utility_onboarding: Option<String>,
    pub utility_credential: Option<String>,
    pub utility_registry: Option<String>,
    pub vault: Option<String>,
    pub vault_governance: Option<String>,
}

// ============================================================================
// Contract / DAR workflow DTOs
// ============================================================================

/// A DAR file to upload
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DarFile {
    /// Filename (used as description when uploading)
    pub filename: String,
    /// Base64-encoded DAR file contents
    pub data: String,
}

/// Definition of a Daml contract to create on the ledger
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ContractDefinition {
    /// Unique identifier for this contract (used as command ID)
    pub id: String,
    /// Human-readable name for logging
    pub name: String,
    /// Package ID (can use # prefix for symbolic lookup)
    pub package_id: String,
    /// Module name (e.g., "CBTC.Governance")
    pub module_name: String,
    /// Entity/template name (e.g., "CBTCGovernanceRules")
    pub entity_name: String,
    /// Record fields for the create command
    pub fields: Vec<FieldDefinition>,
}

/// Definition of a field value in a Daml record
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(no_recursion))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldDefinition {
    /// The decentralized party ID
    DecentralizedParty,
    /// The operator party ID
    OperatorParty,
    /// A specific party ID
    ParticipantParty { id: CantonId },
    /// Static text value
    Text { value: String },
    /// Integer value
    Int64 { value: i64 },
    /// Boolean value
    Bool { value: bool },
    /// The instrument record (admin party + instrument id)
    Instrument { id: String },
    /// Set of all participant parties (as GenMap<Party, Unit>)
    AttestorsSet,
    /// Set of parties (as DA.Set.Types:Set Party - Record wrapped GenMap)
    PartySet { parties: Vec<CantonId> },
    /// Relative time value (as DA.Time.Types:RelTime - Record wrapped Int64 microseconds)
    RelTime { microseconds: i64 },
    /// Optional wrapper around another field
    Optional { inner: Box<FieldDefinition> },
    /// Nested record with fields
    Record { fields: Vec<FieldDefinition> },
    /// Governance threshold value
    GovernanceThreshold { value: Option<i64> },
    /// `None` value for any `Optional<T>` field — emits a typed-empty Optional.
    None,
}

// ============================================================================
// Decentralized party / workflow request + response DTOs
// ============================================================================

/// Where the response data came from
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub enum ResponseSource {
    /// Fresh data from Canton gRPC
    #[default]
    Live,
    /// Cached data from local database
    Cache,
}

/// Response for the decentralized parties endpoint
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DecentralizedPartiesResponse {
    pub parties: Vec<DecentralizedParty>,
    #[serde(default)]
    pub source: ResponseSource,
    /// Whether a background refresh from Canton is currently in progress
    #[serde(default)]
    pub refreshing: bool,
}

/// Request to kick a participant from a decentralized party.
/// `deny_unknown_fields` rejects pre-fix requests carrying
/// `namespace_fingerprint` instead of silently ignoring it (the server now
/// derives it from cache; see `start_kick`).
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct KickRequest {
    pub decentralized_party_id: CantonId,
    pub participant_id: CantonId,
    pub new_threshold: i32,
    /// The party's threshold *before* the kick. Display-only — surfaced on
    /// the workflow run card as "old → new" so the operator can see the
    /// change at a glance. Defaults to 0 (rendered as just the new value)
    /// when an older client omits it.
    #[serde(default)]
    pub previous_threshold: i32,
}

/// Request to create a new decentralized party
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub enum MissingEdgeKind {
    UnreachableFromCoordinator,
    MeshHole,
}

/// One directed missing edge in the peer mesh: `from` does not have `to`
/// configured as a peer (`MeshHole`), or the coordinator could not query
/// `to` at all (`UnreachableFromCoordinator`, `from` is the coordinator).
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct MissingPeerEdge {
    pub from: String,
    pub to: String,
    pub kind: MissingEdgeKind,
}

/// Returned when onboarding pre-flight detects that selected peers are not
/// fully meshed. The workflow is not started.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OnboardingMeshErrorResponse {
    pub error: String,
    pub missing_edges: Vec<MissingPeerEdge>,
}

/// Request to deploy contracts for a decentralized party
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DarsRequest {
    /// DAR files to upload (base64-encoded)
    pub dar_files: Vec<DarFile>,
    /// Peer IDs to distribute to (required non-empty for /dars/distribute, ignored by /dars/upload)
    #[serde(default)]
    pub peer_ids: Vec<CantonId>,
}

/// Response for workflow initiation (kick, onboarding, etc.)
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct WorkflowResponse {
    pub status: WorkflowProgress,
    pub message: String,
}

/// Response wrapper for `GET /workflows`.
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct WorkflowRunsResponse {
    pub runs: Vec<WorkflowRun>,
}

/// Response for key status check
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct KeyStatusResponse {
    pub has_keys: bool,
    pub public_key: Option<String>,
}

/// Payload sent inside an `InviteOnboarding` Noise message.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OnboardingInvitePayload {
    pub prefix: String,
    pub participants: Vec<CantonId>,
    /// The coordinator's `workflow_runs` instance name for this run. Echoed
    /// back in `DeclineInvitationPayload` so the coordinator can tell a
    /// decline of THIS run apart from a stale invite of an earlier run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Payload sent inside a `DeclineInvitation` Noise message — peer telling
/// the coordinator that it has rejected an outstanding invitation so the
/// coordinator can fail its matching in-progress run with a clear error.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DeclineInvitationPayload {
    pub kind: WorkflowKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The coordinator run this decline targets (from the invite payload).
    /// `None` when the invite predates this field — the coordinator then
    /// falls back to kind + membership checks only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Payload sent inside an `InviteDars` Noise message.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DarsInvitePayload {
    pub dar_filenames: Vec<String>,
    /// The member set (selected peers) this distribution targets, so the peer
    /// card can render the same participant list the coordinator shows.
    #[serde(default)]
    pub participants: Vec<CantonId>,
    /// The coordinator's run instance name (see `OnboardingInvitePayload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Payload sent inside an `InviteKick` Noise message — gives the peer enough
/// context to show "kicking X from dec party Y, threshold a→b" before the kick
/// proposals arrive later in the workflow.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct KickInvitePayload {
    pub dec_party_id: CantonId,
    pub kicked_participant: CantonId,
    pub new_threshold: i32,
    pub previous_threshold: i32,
    /// The surviving member set the kick targets, so the peer card renders
    /// the same participant list the coordinator shows.
    #[serde(default)]
    pub participants: Vec<CantonId>,
    /// The coordinator's run instance name (see `OnboardingInvitePayload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Payload sent inside an `InviteContracts` Noise message — mirrors the rich
/// Kick payload so the peer card can show the dec party, member set, and the
/// package/contract names being deployed before the proposals arrive.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ContractsInvitePayload {
    pub dec_party_id: CantonId,
    #[serde(default)]
    pub participants: Vec<CantonId>,
    /// Human-readable contract/package names (from `ContractDefinition.name`).
    #[serde(default)]
    pub package_names: Vec<String>,
    /// The coordinator's run instance name (see `OnboardingInvitePayload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_instance: Option<String>,
}

/// Response for pending invitations endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PendingInvitationsResponse {
    pub invitations: Vec<PendingInvitation>,
}

/// Request to accept or decline an invitation
#[derive(Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InvitationActionRequest {
    pub id: String,
}

// ============================================================================
// Auth DTOs
// ============================================================================

/// Authentication status for a party
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AuthStatus {
    Authenticated,
    Mock,
    Failed { error: String },
    NotConfigured,
}

/// User rights validation result
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PartyAuthStatus {
    pub dec_party_id: CantonId,
    pub member_party_id: CantonId,
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_realm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth0_domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth0_audience: Option<String>,
    pub status: AuthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights: Option<RightsStatus>,
}

/// Response for the auth status endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AuthStatusResponse {
    pub parties: Vec<PartyAuthStatus>,
}

/// Result of an authentication test
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AuthTestResult {
    pub party_id: CantonId,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response for the auth test endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AuthTestResponse {
    pub results: Vec<AuthTestResult>,
}

/// One participant's member party for a given dec party. `None`
/// member_party_id = peer not configured / unreachable.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct KnownMember {
    pub participant_uid: CantonId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_party_id: Option<CantonId>,
}

/// Response for `GET /governance/known-members`.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct KnownMembersResponse {
    pub members: Vec<KnownMember>,
}

/// Request body for `POST /party-config/discover-member-party`. Mirrors the
/// `PartyConfigRequest` shape: either Keycloak fields or Auth0 fields are
/// supplied, used to mint a one-shot token and look up the authenticated
/// user's primary party from Canton's UserManagementService.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DiscoverMemberPartyRequest {
    #[serde(default)]
    pub keycloak_url: String,
    #[serde(default)]
    pub keycloak_realm: String,
    #[serde(default)]
    pub keycloak_client_id: String,
    #[serde(default)]
    pub keycloak_client_secret: Option<String>,
    #[serde(default)]
    pub keycloak_username: Option<String>,
    #[serde(default)]
    pub keycloak_password: Option<String>,
    #[serde(default)]
    pub auth0_domain: Option<String>,
    #[serde(default)]
    pub auth0_audience: Option<String>,
    #[serde(default)]
    pub auth0_client_id: Option<String>,
    #[serde(default)]
    pub auth0_client_secret: Option<String>,
}

/// Response for `POST /party-config/discover-member-party`. `primary_party`
/// is `None` when Canton's user has no primary party assigned.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DiscoverMemberPartyResponse {
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_party: Option<CantonId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Request to grant the configured user the rights they need to act on a dec party
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GrantRightsResponse {
    /// Refreshed rights status after the grant call
    pub rights: RightsStatus,
}

// ============================================================================
// Governance building-block DTOs (no on-chain decimals)
// ============================================================================

/// Instrument identifier (admin + id)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentId {
    pub admin: String,
    pub id: String,
}

/// Credential claim (subject, property, value)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Claim {
    pub subject: String,
    pub property: String,
    pub value: String,
}

/// Which governance system a request targets
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentAllowance {
    pub id: String,
}

/// Additional identifier for an instrument, supplied at `SetupUtility` time.
/// Mirrors `Utility.Registry.Holding.V0.Types.InstrumentIdentifier` — used to
/// record standard market symbols (e.g. Ticker, ISIN) alongside the primary
/// `instrument_id_text` UUID.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentIdentifier {
    pub source: CantonId,
    pub id: String,
    pub scheme: String,
}

/// A disclosed contract to include in the ledger submission
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DisclosedContractInput {
    pub contract_id: String,
    pub blob: String, // base64-encoded created_event_blob
}

/// Request to expire a stale confirmation
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ExpireConfirmationRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub confirmation_cid: String,
    #[serde(default)]
    pub governance_type: GovernanceType,
}

/// Request to cancel (revoke) own confirmation
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CancelConfirmationRequest {
    pub party_id: CantonId,
    pub confirmation_cid: String,
    #[serde(default)]
    pub governance_type: GovernanceType,
}

/// State of a VaultGovernanceRules contract
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GovernanceState {
    pub contract_id: String,
    pub vault_manager: CantonId,
    pub members: Vec<CantonId>,
    pub threshold: i64,
    // Optional on the wire: older governance rules contracts predate this field.
    // `skip_serializing_if` omits it when None and `default` accepts it missing;
    // ts-rs renders it as an optional TS property via the type's `optional_fields`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_confirmation_timeout_microseconds: Option<i64>,
    /// The package-name ref the active rules contract actually lives under,
    /// e.g. `#governance-core-v0-rc4`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_ref: Option<String>,
    /// True when the active governance-core rules contract was found under an
    /// older package than the configured `governance_core` ref (a fallback
    /// hit) — the party should be migrated to the latest package.
    #[serde(default)]
    pub out_of_date: bool,
}

/// Response for the governance state endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GovernanceStateResponse {
    pub state: Option<GovernanceState>,
}

/// Information about a deployed Vault contract
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct VaultInfo {
    pub contract_id: String,
    pub vault_name: String,
    pub share_symbol: String,
    pub is_paused: bool,
    pub vault_manager: CantonId,
}

/// Response for the vaults endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct VaultsResponse {
    pub vaults: Vec<VaultInfo>,
}

/// Information about a ProviderService contract
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProviderServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub provider: CantonId,
}

/// Response for the provider services endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProviderServicesResponse {
    pub services: Vec<ProviderServiceInfo>,
}

/// Information about a UserService contract
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct UserServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub user: CantonId,
}

/// Response for the user services endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct UserServicesResponse {
    pub services: Vec<UserServiceInfo>,
}

/// Information about an InstrumentConfiguration contract (one "token" the
/// governance party can mint/burn against). `instrument_admin` and
/// `instrument_id` are read off the contract's `defaultIdentifier` field and
/// match the `InstrumentId { admin, id }` shape required by Mint/Burn
/// proposals.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentInfo {
    pub contract_id: String,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
}

/// Response for the instruments endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentsResponse {
    pub instruments: Vec<InstrumentInfo>,
}

/// Active `Splice.Api.Token.TransferInstructionV1:TransferFactory` contract
/// visible to the party. The frontend joins these to the party's holdings by
/// `expected_admin == holding.instrument_admin` to prefill the TransferFactory
/// CID and expected-admin fields on the Transfer Proposal form.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferFactoryInfo {
    pub contract_id: String,
    pub expected_admin: CantonId,
}

/// Response for the transfer-factories endpoint.
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferFactoriesResponse {
    pub transfer_factories: Vec<TransferFactoryInfo>,
}

/// A pending `Utility.Credential.App.V0.Model.Offer:CredentialOffer` visible
/// to the party. The accept-free-credential forms list offers where the party
/// is the `holder`, so the CredentialOffer cid no longer has to be pasted in
/// by hand.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CredentialOfferInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub issuer: CantonId,
    pub holder: CantonId,
    /// The template's `id` field — the credential's identifier.
    pub credential_id: String,
    pub description: String,
    /// True when the offer carries no `billingParams`, i.e. it can be taken
    /// with `CredentialOffer_AcceptFree` (the only direction the
    /// AcceptFreeCredential governance action supports).
    pub is_free: bool,
}

/// Response for the credential offers endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CredentialOffersResponse {
    pub credential_offers: Vec<CredentialOfferInfo>,
}

/// Information about a RegistrarService contract
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RegistrarServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub registrar: CantonId,
}

/// Response for the registrar services endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RegistrarServicesResponse {
    pub services: Vec<RegistrarServiceInfo>,
}

/// A contract ID with its blob
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ContractWithBlob {
    pub contract_id: String,
    pub blob: String,
}

/// DSO network info (amulet rules + DSO party)
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NetworkInfo {
    pub dso_party_id: CantonId,
    pub amulet_rules_cid: String,
    pub amulet_rules_blob: String,
}

/// DA Utility operator info (operator party id)
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OperatorInfo {
    pub party_id: CantonId,
}

/// Count of active `TransferPreapproval` contracts a governance party already
/// has, split by direction. The UI uses this to warn the user that re-issuing
/// a `SetupCcPreapproval` / `SetupTokenPreapproval` proposal is pointless
/// (the on-chain choice would fail when executed).
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferPreapprovalsResponse {
    /// `Splice.Wallet.TransferPreapproval:TransferPreapproval` — Canton Coin
    pub cc: usize,
    /// `Utility.Registry.App.V0.Model.TransferPreapproval:TransferPreapproval`
    pub token: usize,
}

/// Response for the generic contract query endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ContractQueryResponse {
    pub contracts: Vec<ContractWithBlob>,
}

// ============================================================================
// Party-config DTOs
// ============================================================================

/// Request to save or update party configuration. Operators submit *either*
/// the Keycloak shape (url+realm+client_id+...) or the Auth0 shape
/// (auth0_domain+audience+client_id+secret), matching whichever top-level
/// provider gates the frontend.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PartyConfigRequest {
    /// The decentralized party ID
    pub dec_party_id: CantonId,
    /// The member party ID (local to this node)
    pub member_party_id: CantonId,
    /// Canton/Ledger API user ID
    pub user_id: String,
    /// Keycloak server URL
    #[serde(default)]
    pub keycloak_url: String,
    /// Keycloak realm name
    #[serde(default)]
    pub keycloak_realm: String,
    /// OAuth2 client ID
    #[serde(default)]
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
    /// Auth0 tenant domain. When set together with the other auth0_*
    /// fields, supersedes the Keycloak fields.
    #[serde(default)]
    pub auth0_domain: Option<String>,
    /// Auth0 API audience.
    #[serde(default)]
    pub auth0_audience: Option<String>,
    /// Auth0 M2M client ID.
    #[serde(default)]
    pub auth0_client_id: Option<String>,
    /// Auth0 M2M client secret. None = keep existing, "" = clear.
    #[serde(default)]
    pub auth0_client_secret: Option<String>,
    /// Package identifiers for deployed Daml contracts
    #[serde(default)]
    pub packages: PackageConfig,
}

/// Response with party configuration (secrets masked)
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
    /// Auth0 tenant domain (None when Auth0 not configured)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth0_domain: Option<String>,
    /// Auth0 API audience
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth0_audience: Option<String>,
    /// Auth0 M2M client ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth0_client_id: Option<String>,
    /// Whether an Auth0 client secret is configured
    pub has_auth0_client_secret: bool,
    /// Package identifiers for deployed Daml contracts
    pub packages: PackageConfig,
}

// ============================================================================
// Generic + audit DTOs
// ============================================================================

/// Generic error response
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ErrorResponse {
    pub error: String,
}

/// Generic success response
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct MessageResponse {
    pub message: String,
}

/// Generic success boolean response
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct SuccessResponse {
    pub success: bool,
}

/// Response for workflow status check endpoints
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct WorkflowStatusResponse {
    pub status: WorkflowProgress,
    // Omit when absent so the wire matches the generated `error?: string`
    // (without `skip_serializing_if`, `None` serializes as `null`, which the
    // optional TS type does not accept).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response for the governance audit endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AuditLogResponse {
    pub entries: Vec<AuditLogEntry>,
    pub total_returned: usize,
}

/// A single on-chain governance audit entry
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
    #[cfg_attr(feature = "typegen", ts(type = "any"))]
    pub details: serde_json::Value,
}

/// Response for the on-chain governance audit endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ChainAuditResponse {
    pub entries: Vec<ChainAuditEntry>,
    pub total_returned: usize,
}
