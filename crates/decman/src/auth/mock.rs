use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use tokio::sync::RwLock;

use crate::{
    canton_id::CantonId,
    config::{InsecureAuthConfig, PartyCredentials},
};

/// Static mock token. Kept as the fallback if runtime minting ever fails, and
/// as the reference the inbound `MockValidator` recognises. Matches the
/// default-configured runtime token on alg/secret/aud/sub (HS256, secret
/// `unsafe`, aud `https://canton.network.global`, sub `ledger-api-user`) but
/// carries a fixed `iat`, so it is not byte-for-byte identical to a freshly
/// minted one.
pub const MOCK_TOKEN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJodHRwczovL2NhbnRvbi5uZXR3b3JrLmdsb2JhbCIsImlhdCI6MTc2Mzc0ODcwMiwic3ViIjoibGVkZ2VyLWFwaS11c2VyIn0.vpkfH4SoM9AZqbE38W4hrvl3xxy69jYs4u8gveskw9k";

/// Default subject/user id, used by the inbound `MockValidator` and as the
/// fallback if minting fails.
pub const MOCK_USER_ID: &str = "ledger-api-user";

/// Default placeholder member party ID for insecure mode
const MOCK_MEMBER_PARTY_ID: &str =
    "mock-member::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01";

/// Claims for the unsafe HS256 Canton token: audience, subject, and issued-at,
/// with no expiry (Canton's unsafe auth does not require one).
#[derive(Serialize)]
struct UnsafeClaims<'a> {
    aud: &'a str,
    sub: &'a str,
    iat: u64,
}

/// Mint an HS256 Canton token from the given secret/audience/subject.
///
/// # Errors
///
/// Returns a `jsonwebtoken` error if token encoding fails.
fn mint_unsafe_token(
    secret: &str,
    audience: &str,
    subject: &str,
) -> Result<String, jsonwebtoken::errors::Error> {
    let iat = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let claims = UnsafeClaims {
        aud: audience,
        sub: subject,
        iat,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

/// Mint the unsafe token and resolve its subject, from the configured
/// secret/audience/subject. Falls back to the static [`MOCK_TOKEN`] if encoding
/// fails. Never logs the secret.
fn mint_credentials(cfg: &InsecureAuthConfig) -> (String, String) {
    match mint_unsafe_token(&cfg.secret, &cfg.audience, &cfg.subject) {
        Ok(token) => {
            tracing::info!(
                "Minted unsafe Canton HMAC token (aud={}, sub={})",
                cfg.audience,
                cfg.subject
            );
            (token, cfg.subject.clone())
        }
        Err(e) => {
            tracing::error!("Failed to mint unsafe Canton HMAC token: {e}; using static fallback");
            (MOCK_TOKEN.to_string(), MOCK_USER_ID.to_string())
        }
    }
}

/// Mock token manager for insecure mode. Holds a token minted once by the
/// [`MockAuthRegistry`].
pub struct MockTokenManager {
    user_id: String,
    token: String,
    member_party_id: CantonId,
}

impl MockTokenManager {
    fn new(token: String, user_id: String, member_party_id: CantonId) -> Self {
        Self {
            user_id,
            token,
            member_party_id,
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

    /// Get the member party ID
    pub fn member_party_id(&self) -> &CantonId {
        &self.member_party_id
    }
}

/// Mock registry for insecure mode — mints the unsafe token once, then hands
/// out managers that carry each party's configured member_party_id (falling
/// back to a placeholder when no credentials are configured).
pub struct MockAuthRegistry {
    party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    token: String,
    user_id: String,
    fallback_member: CantonId,
}

impl MockAuthRegistry {
    /// Create a registry using the default unsafe token settings.
    pub fn new(party_credentials: Arc<RwLock<Vec<PartyCredentials>>>) -> Self {
        Self::with_config(party_credentials, &InsecureAuthConfig::default())
    }

    /// Create a registry minting the unsafe token from `cfg`.
    pub fn with_config(
        party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
        cfg: &InsecureAuthConfig,
    ) -> Self {
        tracing::info!("Initializing mock authentication (insecure mode)");
        let (token, user_id) = mint_credentials(cfg);
        let fallback_member =
            CantonId::parse(MOCK_MEMBER_PARTY_ID).expect("hardcoded mock member party ID");
        Self {
            party_credentials,
            token,
            user_id,
            fallback_member,
        }
    }

    fn manager_for(&self, member_party_id: CantonId) -> Arc<MockTokenManager> {
        Arc::new(MockTokenManager::new(
            self.token.clone(),
            self.user_id.clone(),
            member_party_id,
        ))
    }

    /// Get mock token manager for a party, using its configured member_party_id
    pub async fn get(&self, party_id: &CantonId) -> Arc<MockTokenManager> {
        let creds = self.party_credentials.read().await;
        let member = creds
            .iter()
            .find(|p| p.dec_party_id == *party_id)
            .map(|c| c.member_party_id.clone())
            .unwrap_or_else(|| self.fallback_member.clone());
        self.manager_for(member)
    }

    /// Get mock token manager by string ID
    pub async fn get_by_str(&self, party_id: &str) -> Arc<MockTokenManager> {
        match CantonId::parse(party_id) {
            Ok(id) => self.get(&id).await,
            Err(_) => self.manager_for(self.fallback_member.clone()),
        }
    }
}
