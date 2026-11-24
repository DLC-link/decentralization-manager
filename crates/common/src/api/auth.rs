//! Auth status, rights, member discovery and rights grants.

use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

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
#[derive(Clone, Debug, Deserialize, Serialize)]
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
