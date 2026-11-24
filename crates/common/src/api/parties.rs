//! Decentralized party lists, former parties, purge and reconnect.

use serde::{Deserialize, Serialize};

use crate::{canton_id::CantonId, types::DecentralizedParty};

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

/// One proposal card in the paged proposals feed.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProposalSummary {
    pub proposal_cid: String,
    /// Live confirmations, deduped to the newest per member and excluding
    /// expired ones — the same count the notifications feed shows.
    #[serde(default)]
    pub confirmation_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposer: Option<CantonId>,
    #[cfg_attr(feature = "typegen", ts(type = "number | null"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
}

/// A batch of proposals plus where to resume.
///
/// `next_cursor` is opaque: hand it back verbatim to get the following batch.
/// `None` means the party has no more open proposals.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProposalsPageResponse {
    pub proposals: Vec<ProposalSummary>,
    /// Confirmations needed to execute, so a card can render "n of threshold".
    #[serde(default)]
    pub threshold: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Response for the decentralized parties endpoint
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DecentralizedPartiesResponse {
    pub parties: Vec<DecentralizedParty>,
    /// Parties this participant hosts, before any paging window is applied.
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub source: ResponseSource,
    /// Whether a background refresh from Canton is currently in progress
    #[serde(default)]
    pub refreshing: bool,
}

/// Where the cleanup of a former party's leftover contracts stands.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub enum PurgeStatus {
    /// No purge has run.
    #[default]
    None,
    Running,
    Succeeded,
    Failed,
}

/// A decentralized party this participant hosted once and no longer hosts.
///
/// The participant keeps the contracts it held for the party, and stops
/// seeing their archives the moment it is removed, so they go stale.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct FormerParty {
    pub party_id: CantonId,
    /// When the first hosting mapping without this participant took effect,
    /// in seconds since the epoch.
    pub removed_at: i64,
    /// When this node noticed the removal, in seconds since the epoch.
    pub detected_at: i64,
    pub purge_status: PurgeStatus,
    #[cfg_attr(feature = "typegen", ts(type = "number | null"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_started_at: Option<i64>,
    #[cfg_attr(feature = "typegen", ts(type = "number | null"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_finished_at: Option<i64>,
    /// Contracts the last successful purge removed.
    #[cfg_attr(feature = "typegen", ts(type = "number | null"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purged_count: Option<i64>,
    /// Contracts of the party the participant keeps, because another party
    /// it still hosts is a stakeholder of them.
    #[cfg_attr(feature = "typegen", ts(type = "number | null"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_error: Option<String>,
}

/// Response for the former parties endpoint
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct FormerPartiesResponse {
    pub parties: Vec<FormerParty>,
}

/// What a purge of a former party would do right now.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PurgePreview {
    pub party_id: CantonId,
    /// Contracts no party this participant still hosts is a stakeholder of.
    pub purge_count: usize,
    /// Contracts another party this participant hosts is a stakeholder of.
    pub keep_count: usize,
}

/// Request to purge a former party's leftover contracts from this participant
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(deny_unknown_fields)]
pub struct PurgeFormerPartyRequest {
    pub party_id: String,
}

/// Request to reconnect this participant after a purge of a former party failed
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(deny_unknown_fields)]
pub struct ReconnectFormerPartyRequest {
    pub party_id: String,
}
