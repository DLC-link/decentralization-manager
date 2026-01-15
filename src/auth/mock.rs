use std::sync::Arc;

use crate::participant_id::CantonId;

/// Static mock token for test mode (from legacy config)
pub const MOCK_TOKEN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJodHRwczovL2NhbnRvbi5uZXR3b3JrLmdsb2JhbCIsImlhdCI6MTc2Mzc0ODcwMiwic3ViIjoibGVkZ2VyLWFwaS11c2VyIn0.vpkfH4SoM9AZqbE38W4hrvl3xxy69jYs4u8gveskw9k";

/// Static mock user ID for test mode (from legacy config)
pub const MOCK_USER_ID: &str = "ledger-api-user";

/// Mock token manager for test mode
pub struct MockTokenManager {
    user_id: String,
    token: String,
}

impl MockTokenManager {
    /// Create a new MockTokenManager with default test credentials
    pub fn new() -> Self {
        Self {
            user_id: MOCK_USER_ID.to_string(),
            token: MOCK_TOKEN.to_string(),
        }
    }

    /// Get the user ID
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Get the mock token (always succeeds)
    pub fn get_token(&self) -> String {
        self.token.clone()
    }
}

impl Default for MockTokenManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Mock registry for test mode - returns static tokens for all parties
pub struct MockAuthRegistry {
    manager: Arc<MockTokenManager>,
}

impl MockAuthRegistry {
    /// Create a new MockAuthRegistry
    pub fn new() -> Self {
        tracing::info!("Initializing mock authentication (test mode)");
        Self {
            manager: Arc::new(MockTokenManager::new()),
        }
    }

    /// Get mock token manager (returns the same manager for any party)
    pub fn get(&self, _party_id: &CantonId) -> Arc<MockTokenManager> {
        self.manager.clone()
    }

    /// Get mock token manager by string ID
    pub fn get_by_str(&self, _party_id: &str) -> Arc<MockTokenManager> {
        self.manager.clone()
    }
}

impl Default for MockAuthRegistry {
    fn default() -> Self {
        Self::new()
    }
}
