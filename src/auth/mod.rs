mod mock;

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use keycloak::login::{PasswordParams, RefreshParams};
use tokio::sync::RwLock;

pub use mock::{MockAuthRegistry, MockTokenManager};

use crate::{
    config::{KeycloakConfig, PartyCredentials},
    error::Result,
    participant_id::CantonId,
};

struct TokenState {
    access_token: String,
    refresh_token: String,
    expires_at: SystemTime,
}

/// Manages Keycloak token lifecycle with automatic refresh for a single party
pub struct TokenManager {
    config: KeycloakConfig,
    user_id: String,
    /// The member party ID that owns these credentials
    member_party_id: CantonId,
    state: RwLock<TokenState>,
}

impl TokenManager {
    /// Create a new TokenManager and perform initial authentication
    ///
    /// # Errors
    ///
    /// Returns an error if Keycloak authentication fails
    pub async fn new(
        config: KeycloakConfig,
        user_id: String,
        member_party_id: CantonId,
    ) -> Result<Self> {
        let state = Self::authenticate(&config).await?;
        Ok(Self {
            config,
            user_id,
            member_party_id,
            state: RwLock::new(state),
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

    async fn authenticate(config: &KeycloakConfig) -> Result<TokenState> {
        let url = keycloak::login::password_url(&config.url, &config.realm);
        let response = keycloak::login::password(PasswordParams {
            client_id: config.client_id.clone(),
            username: config.username.clone(),
            password: config.password.clone(),
            url,
        })
        .await
        .map_err(|e| anyhow::anyhow!("Keycloak authentication failed: {e}"))?;

        let expires_in_secs = (response.expires_in.saturating_sub(60)) as u64;
        let expires_at = SystemTime::now()
            .checked_add(Duration::from_secs(expires_in_secs))
            .unwrap_or(SystemTime::now());

        Ok(TokenState {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_at,
        })
    }

    async fn refresh_or_reauthenticate(&self) -> Result<()> {
        let mut state = self.state.write().await;
        let url = keycloak::login::password_url(&self.config.url, &self.config.realm);

        match keycloak::login::refresh(RefreshParams {
            client_id: self.config.client_id.clone(),
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
                *state = Self::authenticate(&self.config).await?;
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Token refresh failed: {e}"));
            }
        }

        Ok(())
    }
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
    /// Returns an error if Keycloak authentication fails for any party
    pub async fn new(parties: &[PartyCredentials]) -> Result<Self> {
        let mut managers = HashMap::new();

        for party in parties {
            let dec_party_id = party.dec_party_id.to_string();
            tracing::info!(
                "Initializing authentication for dec_party={dec_party_id}, member_party={}",
                party.member_party_id
            );

            let manager = TokenManager::new(
                party.keycloak.clone(),
                party.user_id.clone(),
                party.member_party_id.clone(),
            )
            .await?;

            managers.insert(dec_party_id, Arc::new(manager));
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

/// Unified auth provider that works with workflows
/// Supports both real Keycloak auth and mock auth for testing
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
                let tm = registry.get(dec_party_id).ok_or_else(|| {
                    anyhow::anyhow!("No credentials configured for party: {dec_party_id}")
                })?;
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
                let mm = registry.get(dec_party_id);
                Ok(PartyAuthCredentials {
                    token: mm.get_token(),
                    user_id: mm.user_id().to_string(),
                    member_party_id: mm.member_party_id().clone(),
                })
            }
        }
    }
}
