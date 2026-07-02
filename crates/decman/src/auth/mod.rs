mod validators;

pub(crate) mod mock;

pub mod validator;

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use keycloak::login::{ClientCredentialsParams, PasswordParams, RefreshParams};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::RwLock;

pub use mock::{MockAuthRegistry, MockTokenManager};
pub use validator::{Principal, TokenValidator, ValidationError};
pub use validators::{JwtValidator, MockValidator, OidcIntrospectionValidator};

use crate::{
    canton_id::CantonId,
    config::{Auth0M2MConfig, KeycloakConfig, PartyCredentials},
};

/// Authentication errors
#[derive(Error, Debug)]
pub enum AuthError {
    #[error("Keycloak M2M authentication failed: {0}")]
    M2MAuthFailed(String),

    #[error("Keycloak password authentication failed: {0}")]
    PasswordAuthFailed(String),

    #[error("Token refresh failed: {0}")]
    RefreshFailed(String),

    #[error("Missing username for password flow")]
    MissingUsername,

    #[error("Missing password for password flow")]
    MissingPassword,

    #[error("No credentials configured for party: {0}")]
    NoCredentials(String),
}

type Result<T> = std::result::Result<T, AuthError>;

struct TokenState {
    access_token: String,
    /// Refresh token (empty for M2M/client_credentials flow)
    refresh_token: String,
    expires_at: SystemTime,
    /// Whether this is using M2M auth (no refresh token available)
    is_m2m: bool,
}

/// Source of OAuth2 tokens for a party. Each variant knows how to mint
/// access tokens for the dec party on Canton.
#[derive(Clone, Debug)]
enum TokenSource {
    Keycloak(KeycloakConfig),
    Auth0(Auth0M2MConfig),
}

/// Manages OAuth2 token lifecycle with automatic refresh for a single party.
/// Supports both Keycloak and Auth0 M2M.
pub struct TokenManager {
    source: TokenSource,
    user_id: String,
    /// The member party ID that owns these credentials
    member_party_id: CantonId,
    state: RwLock<TokenState>,
    http: reqwest::Client,
}

impl TokenManager {
    /// Create a TokenManager from a Keycloak config and perform initial auth.
    ///
    /// # Errors
    ///
    /// Returns an error if Keycloak authentication fails
    pub async fn new(
        config: KeycloakConfig,
        user_id: String,
        member_party_id: CantonId,
    ) -> Result<Self> {
        let source = TokenSource::Keycloak(config);
        let http = reqwest::Client::new();
        let state = Self::authenticate(&source, &http).await?;
        Ok(Self {
            source,
            user_id,
            member_party_id,
            state: RwLock::new(state),
            http,
        })
    }

    /// Create a TokenManager from an Auth0 M2M config and perform initial auth.
    ///
    /// # Errors
    ///
    /// Returns an error if Auth0 authentication fails
    pub async fn new_auth0(
        config: Auth0M2MConfig,
        user_id: String,
        member_party_id: CantonId,
    ) -> Result<Self> {
        let source = TokenSource::Auth0(config);
        let http = reqwest::Client::new();
        let state = Self::authenticate(&source, &http).await?;
        Ok(Self {
            source,
            user_id,
            member_party_id,
            state: RwLock::new(state),
            http,
        })
    }

    /// Get the user ID for this party's credentials
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Get the member party ID that owns these credentials
    pub fn member_party_id(&self) -> &CantonId {
        &self.member_party_id
    }

    /// Get a fresh access token, refreshing if necessary
    ///
    /// # Errors
    ///
    /// Returns an error if token refresh or re-authentication fails
    pub async fn get_token(&self) -> Result<String> {
        let needs_refresh = {
            let state = self.state.read().await;
            SystemTime::now() >= state.expires_at
        };

        if needs_refresh {
            self.refresh_or_reauthenticate().await?;
        }

        let state = self.state.read().await;
        Ok(state.access_token.clone())
    }

    async fn authenticate(source: &TokenSource, http: &reqwest::Client) -> Result<TokenState> {
        match source {
            TokenSource::Keycloak(config) => Self::authenticate_keycloak(config).await,
            TokenSource::Auth0(config) => Self::authenticate_auth0(config, http).await,
        }
    }

    async fn authenticate_keycloak(config: &KeycloakConfig) -> Result<TokenState> {
        let url = keycloak::login::token_url(&config.url, &config.realm);

        // Choose auth method: client_credentials (M2M) if client_secret is set, otherwise password flow
        let (response, is_m2m) = if let Some(ref client_secret) = config.client_secret {
            tracing::debug!("Using client_credentials (M2M) auth flow");
            let response = keycloak::login::client_credentials(ClientCredentialsParams {
                url,
                client_id: config.client_id.clone(),
                client_secret: client_secret.clone(),
            })
            .await
            .map_err(AuthError::M2MAuthFailed)?;
            (response, true)
        } else {
            // Password flow requires username and password
            let username = config.username.as_ref().ok_or(AuthError::MissingUsername)?;
            let password = config.password.as_ref().ok_or(AuthError::MissingPassword)?;

            tracing::debug!("Using password auth flow");
            let response = keycloak::login::password(PasswordParams {
                client_id: config.client_id.clone(),
                username: username.clone(),
                password: password.clone(),
                url,
            })
            .await
            .map_err(AuthError::PasswordAuthFailed)?;
            (response, false)
        };

        let expires_in_secs = (response.expires_in.saturating_sub(60)) as u64;
        let expires_at = SystemTime::now()
            .checked_add(Duration::from_secs(expires_in_secs))
            .unwrap_or(SystemTime::now());

        Ok(TokenState {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_at,
            is_m2m,
        })
    }

    async fn authenticate_auth0(
        config: &Auth0M2MConfig,
        http: &reqwest::Client,
    ) -> Result<TokenState> {
        let response = auth0_client_credentials(http, config).await?;
        let expires_in_secs = response.expires_in.saturating_sub(60);
        let expires_at = SystemTime::now()
            .checked_add(Duration::from_secs(expires_in_secs))
            .unwrap_or(SystemTime::now());
        Ok(TokenState {
            access_token: response.access_token,
            refresh_token: String::new(),
            expires_at,
            is_m2m: true,
        })
    }

    async fn refresh_or_reauthenticate(&self) -> Result<()> {
        let mut state = self.state.write().await;

        // M2M auth doesn't have refresh tokens, just re-authenticate
        if state.is_m2m {
            tracing::debug!("M2M token expired, re-authenticating");
            *state = Self::authenticate(&self.source, &self.http).await?;
            return Ok(());
        }

        // Password flow only applies to Keycloak source
        let TokenSource::Keycloak(ref config) = self.source else {
            *state = Self::authenticate(&self.source, &self.http).await?;
            return Ok(());
        };

        let url = keycloak::login::token_url(&config.url, &config.realm);

        match keycloak::login::refresh(RefreshParams {
            client_id: config.client_id.clone(),
            refresh_token: state.refresh_token.clone(),
            url,
        })
        .await
        {
            Ok(response) => {
                let expires_in_secs = (response.expires_in.saturating_sub(60)) as u64;
                state.access_token = response.access_token;
                state.refresh_token = response.refresh_token;
                state.expires_at = SystemTime::now()
                    .checked_add(Duration::from_secs(expires_in_secs))
                    .unwrap_or(SystemTime::now());
            }
            Err(e) if e.contains("Token is not active") => {
                tracing::warn!("Refresh token expired, re-authenticating");
                *state = Self::authenticate(&self.source, &self.http).await?;
            }
            Err(e) => {
                return Err(AuthError::RefreshFailed(e));
            }
        }

        Ok(())
    }
}

/// Auth0 /oauth/token client_credentials response shape.
#[derive(Deserialize)]
pub struct Auth0TokenResponse {
    pub access_token: String,
    pub expires_in: u64,
}

/// Mint an access token via Auth0's client_credentials flow.
///
/// # Errors
///
/// Returns `AuthError::M2MAuthFailed` if the token endpoint is unreachable
/// or rejects the credentials.
pub(crate) async fn auth0_client_credentials(
    http: &reqwest::Client,
    config: &Auth0M2MConfig,
) -> Result<Auth0TokenResponse> {
    let token_url = format!(
        "https://{}/oauth/token",
        config.domain.trim_end_matches('/')
    );

    let body = serde_json::json!({
        "grant_type": "client_credentials",
        "client_id": config.client_id,
        "client_secret": config.client_secret,
        "audience": config.audience,
    });

    let response = http
        .post(&token_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| AuthError::M2MAuthFailed(format!("Auth0 request failed: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::M2MAuthFailed(format!(
            "Auth0 token endpoint returned {status}: {body}"
        )));
    }

    response
        .json::<Auth0TokenResponse>()
        .await
        .map_err(|e| AuthError::M2MAuthFailed(format!("Auth0 response parse failed: {e}")))
}

/// Registry of TokenManagers for multiple parties
pub struct AuthRegistry {
    managers: HashMap<String, Arc<TokenManager>>,
}

impl AuthRegistry {
    /// Create a new AuthRegistry and initialize TokenManagers for all configured parties
    ///
    /// # Errors
    ///
    /// Returns an error if authentication fails for any party
    pub async fn new(parties: &[PartyCredentials]) -> Result<Self> {
        let mut managers = HashMap::new();

        for party in parties {
            let dec_party_id = party.dec_party_id.to_string();
            tracing::info!(
                "Initializing authentication for dec_party={dec_party_id}, member_party={}",
                party.member_party_id
            );

            let result = if let Some(ref auth0) = party.auth0 {
                TokenManager::new_auth0(
                    auth0.clone(),
                    party.user_id.clone(),
                    party.member_party_id.clone(),
                )
                .await
            } else {
                TokenManager::new(
                    party.keycloak.clone(),
                    party.user_id.clone(),
                    party.member_party_id.clone(),
                )
                .await
            };

            match result {
                Ok(manager) => {
                    managers.insert(dec_party_id, Arc::new(manager));
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to initialize auth for dec_party={dec_party_id}: {e}. \
                         Skipping — workflows for this party will fail until credentials are fixed."
                    );
                }
            }
        }

        Ok(Self { managers })
    }

    /// Get TokenManager for a specific party
    pub fn get(&self, party_id: &CantonId) -> Option<Arc<TokenManager>> {
        self.managers.get(&party_id.to_string()).cloned()
    }

    /// Get TokenManager for a specific party by string ID
    pub fn get_by_str(&self, party_id: &str) -> Option<Arc<TokenManager>> {
        self.managers.get(party_id).cloned()
    }

    /// Check if credentials are configured for a party
    pub fn has_credentials(&self, party_id: &CantonId) -> bool {
        self.managers.contains_key(&party_id.to_string())
    }

    /// Get all configured party IDs
    pub fn party_ids(&self) -> Vec<&String> {
        self.managers.keys().collect()
    }
}

/// Unified auth provider that works with workflows.
/// Supports real OAuth2 auth (Keycloak or Auth0 M2M, via the auth registry) and
/// mock auth for testing.
#[derive(Clone)]
pub enum WorkflowAuth {
    Keycloak(Arc<AuthRegistry>),
    Mock(Arc<MockAuthRegistry>),
}

/// Credentials for a party, including token, user_id, and member_party_id
pub struct PartyAuthCredentials {
    pub token: String,
    pub user_id: String,
    pub member_party_id: CantonId,
}

impl WorkflowAuth {
    /// Get credentials for a decentralized party
    ///
    /// Returns token, user_id, and member_party_id.
    /// The member_party_id is the local party that owns the credentials and can
    /// act_as/read_as both itself and the decentralized party.
    pub async fn get_credentials(&self, dec_party_id: &CantonId) -> Result<PartyAuthCredentials> {
        match self {
            WorkflowAuth::Keycloak(registry) => {
                let tm = registry
                    .get(dec_party_id)
                    .ok_or_else(|| AuthError::NoCredentials(dec_party_id.to_string()))?;
                let token = tm.get_token().await?;
                let user_id = tm.user_id().to_string();
                let member_party_id = tm.member_party_id().clone();
                Ok(PartyAuthCredentials {
                    token,
                    user_id,
                    member_party_id,
                })
            }
            WorkflowAuth::Mock(registry) => {
                let mm = registry.get(dec_party_id).await;
                Ok(PartyAuthCredentials {
                    token: mm.get_token(),
                    user_id: mm.user_id().to_string(),
                    member_party_id: mm.member_party_id().clone(),
                })
            }
        }
    }
}
