use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{config::PartyCredentials, participant_id::CantonId};

/// Static mock token for test mode (from legacy config)
pub const MOCK_TOKEN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJodHRwczovL2NhbnRvbi5uZXR3b3JrLmdsb2JhbCIsImlhdCI6MTc2Mzc0ODcwMiwic3ViIjoibGVkZ2VyLWFwaS11c2VyIn0.vpkfH4SoM9AZqbE38W4hrvl3xxy69jYs4u8gveskw9k";

/// Static mock user ID for test mode (from legacy config)
pub const MOCK_USER_ID: &str = "ledger-api-user";

/// Default placeholder member party ID for test mode
const MOCK_MEMBER_PARTY_ID: &str =
    "mock-member::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";

/// Mock token manager for test mode
pub struct MockTokenManager {
    user_id: String,
    token: String,
    member_party_id: CantonId,
}

impl MockTokenManager {
    /// Create a new MockTokenManager with a specific member party ID
    pub fn with_member_party_id(member_party_id: CantonId) -> Self {
        Self {
            user_id: MOCK_USER_ID.to_string(),
            token: MOCK_TOKEN.to_string(),
            member_party_id,
        }
    }

    /// Create a new MockTokenManager with the default placeholder member party ID
    pub fn new() -> Self {
        let member_party_id =
            CantonId::parse(MOCK_MEMBER_PARTY_ID).expect("hardcoded mock member party ID");
        Self::with_member_party_id(member_party_id)
    }

    /// Get the user ID
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Get the mock token (always succeeds)
    pub fn get_token(&self) -> String {
        self.token.clone()
    }

    /// Get the member party ID
    pub fn member_party_id(&self) -> &CantonId {
        &self.member_party_id
    }
}

impl Default for MockTokenManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Mock registry for test mode — looks up member_party_id from party credentials,
/// falling back to a default placeholder when no credentials are configured.
pub struct MockAuthRegistry {
    party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    fallback: Arc<MockTokenManager>,
}

impl MockAuthRegistry {
    /// Create a new MockAuthRegistry backed by the live party credentials
    pub fn new(party_credentials: Arc<RwLock<Vec<PartyCredentials>>>) -> Self {
        tracing::info!("Initializing mock authentication (test mode)");
        Self {
            party_credentials,
            fallback: Arc::new(MockTokenManager::new()),
        }
    }

    /// Get mock token manager for a party, using its configured member_party_id
    pub async fn get(&self, party_id: &CantonId) -> Arc<MockTokenManager> {
        let creds = self.party_credentials.read().await;
        match creds.iter().find(|p| p.dec_party_id == *party_id) {
            Some(c) => Arc::new(MockTokenManager::with_member_party_id(
                c.member_party_id.clone(),
            )),
            None => self.fallback.clone(),
        }
    }

    /// Get mock token manager by string ID
    pub async fn get_by_str(&self, party_id: &str) -> Arc<MockTokenManager> {
        match CantonId::parse(party_id) {
            Ok(id) => self.get(&id).await,
            Err(_) => self.fallback.clone(),
        }
    }
}
