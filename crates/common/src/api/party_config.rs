//! Party-config request and response.

use serde::{Deserialize, Serialize};

use super::PackageConfig;
use crate::canton_id::CantonId;

/// Request to save or update party configuration. Operators submit *either*
/// the Keycloak shape (url+realm+client_id+...) or the Auth0 shape
/// (auth0_domain+audience+client_id+secret), matching whichever top-level
/// provider gates the frontend.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(deny_unknown_fields)]
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
