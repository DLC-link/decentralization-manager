//! Workflow start requests and their responses: kick, add party, threshold,
//! onboarding, contracts and DARs.

use serde::{Deserialize, Serialize};

use super::{ContractDefinition, DarFile};
use crate::{
    canton_id::CantonId,
    types::{WorkflowProgress, WorkflowRun},
};

/// Request to kick a participant from a decentralized party.
/// `deny_unknown_fields` rejects pre-fix requests carrying
/// `namespace_fingerprint` instead of silently ignoring it (the server now
/// derives it from cache; see `start_kick`).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct KickRequest {
    pub decentralized_party_id: CantonId,
    /// The member to remove. A member that hosts the party is named by its
    /// participant. A member that only owns the namespace can be named by its
    /// participant when a peer has claimed its key, or by `owner_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant_id: Option<CantonId>,
    /// The namespace owner key to remove. An owner key that no peer has
    /// claimed has no participant, so this is the only way to name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_key: Option<String>,
    pub new_threshold: i32,
    /// The party's threshold *before* the kick. Display-only — surfaced on
    /// the workflow run card as "old → new" so the operator can see the
    /// change at a glance. Defaults to 0 (rendered as just the new value)
    /// when an older client omits it.
    #[serde(default)]
    pub previous_threshold: i32,
}

/// Request to add a new member to an existing decentralized party.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AddPartyRequest {
    pub decentralized_party_id: CantonId,
    pub new_participant_id: CantonId,
    pub new_threshold: i32,
    /// The party's threshold *before* the add. Display-only — surfaced on
    /// the workflow run card as "old → new". Defaults to 0 (rendered as
    /// just the new value) when an older client omits it.
    #[serde(default)]
    pub previous_threshold: i32,
}

/// Request to change the thresholds of an existing decentralized party.
/// Re-issues the namespace + party mappings; membership is unchanged.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ChangeThresholdRequest {
    pub decentralized_party_id: CantonId,
    /// How many namespace owners must sign a topology change. The party's
    /// signing keys use the same threshold.
    pub new_threshold: i32,
    /// How many hosting participants must confirm a transaction for the
    /// party. Omitted means the same value as `new_threshold`.
    #[serde(default)]
    pub confirmation_threshold: Option<i32>,
    /// The party's threshold *before* the change. Display-only — surfaced on
    /// the workflow run card as "old → new". Defaults to 0 (rendered as just
    /// the new value) when an older client omits it.
    #[serde(default)]
    pub previous_threshold: i32,
}

/// Request to create a new decentralized party
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OnboardingRequest {
    /// Party ID prefix for the decentralized party (e.g., "xyz-network")
    pub party_id_prefix: String,
    /// List of peer IDs to invite to the decentralized party
    pub peer_ids: Vec<CantonId>,
    /// Initial namespace threshold for the new party: how many owners must
    /// sign a topology change. The party's signing keys use the same value.
    /// Optional: when omitted the coordinator falls back to the default
    /// majority algorithm (`ceil(owner_count / 2)`, min 1). The frontend
    /// pre-fills this with the same default so the operator can see and
    /// adjust it.
    #[serde(default)]
    pub threshold: Option<i32>,
    /// How many hosting participants must confirm a transaction for the new
    /// party. Omitted means the same value as the namespace threshold.
    #[serde(default)]
    pub confirmation_threshold: Option<i32>,
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

/// Request to distribute a DAR this node already holds, named by one of its
/// packages
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DistributePackageRequest {
    /// A package id in the DAR to distribute
    pub package_id: String,
    /// Peer IDs to distribute to (required non-empty)
    pub peer_ids: Vec<CantonId>,
}

/// Response for workflow initiation (kick, onboarding, etc.)
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct WorkflowResponse {
    pub status: WorkflowProgress,
    pub message: String,
    /// The freshly started run's `instance_name`, so callers can address it
    /// directly — with concurrent runs of the same kind, the legacy per-kind
    /// status/cancel endpoints are no longer unambiguous.
    pub instance_name: String,
}

/// Response wrapper for `GET /workflows`.
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct WorkflowRunsResponse {
    pub runs: Vec<WorkflowRun>,
}
