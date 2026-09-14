//! Wire DTOs for the Canton-native coordination surface: the node identity
//! (`GET`/`PUT /node-identity`) and the on-ledger registry (`GET /registry`).
//!
//! Pure data-transfer types, like [`crate::api`]: no server-only
//! dependencies, `utoipa` behind `openapi`, `ts-rs` behind `typegen`.

use serde::{Deserialize, Serialize};

use crate::{canton_id::CantonId, types::Permission};

/// Body of `PUT /node-identity`. The same shape as `PartyConfigRequest`
/// without `dec_party_id`: the node party takes the member party's place, and
/// the row stores it in both columns.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NodeIdentityRequest {
    /// The node party. Must be hosted on this participant with Submission
    /// permission.
    pub node_party_id: CantonId,
    /// Ledger API user with `CanActAs` and `CanReadAs` on the node party.
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
}

/// Response of `GET /node-identity` and of a successful `PUT`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NodeIdentityResponse {
    /// Whether a `kind = 'node'` credentials row exists.
    pub configured: bool,
    /// The node party, when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_party_id: Option<CantonId>,
    /// This node's Canton participant id.
    pub participant_id: CantonId,
    /// The Ledger API user of the node party, when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// How this participant hosts the node party in the synchronizer head
    /// state. `None` when not configured or when the topology read failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosting_permission: Option<Permission>,
}

/// How fresh a peer's registry entry is, as the observer loop last saw it.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "PascalCase")]
pub enum PeerHealthStatus {
    /// A registry entry is visible and its last heartbeat is recent.
    Active,
    /// A registry entry is visible but `now - lastActiveAt` exceeds the
    /// stale factor times the peer's heartbeat interval.
    Stale,
    /// The peer has vetted the coordination package but no entry signed by
    /// its node party is visible to this node.
    Unknown,
    /// The peer's participant has not vetted the coordination package, so it
    /// cannot see or publish registry entries.
    Unvetted,
}

/// One `DecmanNode` contract as this node sees it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DecmanNodeView {
    /// The contract id of the current entry.
    pub contract_id: String,
    /// The signatory node party. Registry reads are keyed by this field.
    pub node_party: CantonId,
    /// The participant the entry claims to run on. Cross-checked against the
    /// synchronizer topology before use; `hosting_verified` says whether the
    /// check passed.
    pub participant_id: String,
    /// Whether the topology store confirms `node_party` is hosted on
    /// `participant_id` with Submission permission.
    pub hosting_verified: bool,
    pub display_name: String,
    /// decman semver the peer runs.
    pub version: String,
    /// Display build identity (image tag / short SHA / `<semver>-dev`).
    pub build_version: String,
    pub coordination_version: i64,
    /// The node parties the publisher named as observers.
    pub peers: Vec<CantonId>,
    /// Unix seconds of the last heartbeat.
    pub last_active_at: i64,
    pub heartbeat_interval_secs: i64,
    pub min_heartbeat_interval_secs: i64,
    /// Seconds since `last_active_at` at the time of the read.
    pub heartbeat_age_secs: i64,
    pub status: PeerHealthStatus,
}

/// Response of `GET /registry`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RegistryResponse {
    /// This node's own entry, when published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_entry: Option<DecmanNodeView>,
    /// Entries signed by a node party that is in this node's peers table.
    pub peers: Vec<DecmanNodeView>,
    /// Entries visible to this node whose signatory is not a configured peer:
    /// another operator added this node before this operator added them.
    pub inbound: Vec<DecmanNodeView>,
}
