use std::time::{Duration, Instant};

use anyhow::Context;
use reqwest::Client;

const REFRESH_MARGIN: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct KeycloakCreds {
    pub url: String,
    pub realm: String,
    pub client_id: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug)]
pub(crate) struct TokenState {
    access_token: String,
    expires_at: Instant,
}

impl TokenState {
    /// Construct an initially-expired state so the first .token() call refreshes.
    pub(crate) fn expired() -> Self {
        Self {
            access_token: String::new(),
            expires_at: Instant::now()
                .checked_sub(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
        }
    }
}

#[derive(Debug)]
pub enum Refresher {
    /// Localnet: a static token. MockValidator accepts any value.
    Static { token: String },
    /// Devnet: re-fetches via Keycloak password grant when expiry is near.
    Keycloak {
        client: Client,
        creds: KeycloakCreds,
        state: tokio::sync::Mutex<TokenState>,
    },
}

impl Refresher {
    pub async fn token(&self) -> anyhow::Result<String> {
        match self {
            Self::Static { token } => Ok(token.clone()),
            Self::Keycloak { client, creds, state } => {
                let mut s = state.lock().await;
                if s.expires_at
                    .checked_duration_since(Instant::now())
                    .map(|d| d < REFRESH_MARGIN)
                    .unwrap_or(true)
                {
                    *s = fetch_token(client, creds).await?;
                }
                Ok(s.access_token.clone())
            }
        }
    }
}

async fn fetch_token(client: &Client, creds: &KeycloakCreds) -> anyhow::Result<TokenState> {
    // Tolerate base URLs configured both with and without a trailing `/auth`.
    // Mirrors src/auth/mod.rs::token_url so the test runner and DPM agree on
    // the endpoint regardless of how DECPM_KEYCLOAK_URL is set.
    let kc_base = creds.url.trim_end_matches('/');
    let kc_base = kc_base.strip_suffix("/auth").unwrap_or(kc_base);
    let url = format!(
        "{kc_base}/auth/realms/{realm}/protocol/openid-connect/token",
        realm = creds.realm,
    );
    let resp: serde_json::Value = client
        .post(&url)
        .form(&[
            ("grant_type", "password"),
            ("client_id", creds.client_id.as_str()),
            ("username", creds.username.as_str()),
            ("password", creds.password.as_str()),
        ])
        .send()
        .await
        .context("Keycloak token POST")?
        .error_for_status()
        .context("Keycloak token status")?
        .json()
        .await
        .context("Keycloak token JSON")?;
    let access_token = resp["access_token"]
        .as_str()
        .context("Keycloak response missing access_token")?
        .to_string();
    let expires_in = resp["expires_in"]
        .as_u64()
        .context("Keycloak response missing expires_in")?;
    Ok(TokenState {
        access_token,
        expires_at: Instant::now() + Duration::from_secs(expires_in),
    })
}

#[cfg(test)]
mod tests {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;

    #[tokio::test]
    async fn static_returns_same_token_every_call() {
        let r = Refresher::Static {
            token: "abc".to_string(),
        };
        let t1 = r.token().await.unwrap();
        let t2 = r.token().await.unwrap();
        assert_eq!(t1, "abc");
        assert_eq!(t2, "abc");
    }

    #[tokio::test]
    async fn keycloak_fetches_when_expired_then_caches() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/auth/realms/test-realm/protocol/openid-connect/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "fresh-1",
                    "expires_in": 3600
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let creds = KeycloakCreds {
            url: server.uri(),
            realm: "test-realm".to_string(),
            client_id: "test-client".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
        };
        let r = Refresher::Keycloak {
            client: Client::new(),
            creds,
            state: tokio::sync::Mutex::new(TokenState::expired()),
        };

        // First call: fetches from Keycloak.
        let t1 = r.token().await.unwrap();
        assert_eq!(t1, "fresh-1");

        // Second call: token is cached (expires_in=3600, well above 30s threshold).
        let t2 = r.token().await.unwrap();
        assert_eq!(t2, "fresh-1");

        // Verify mock received exactly one POST.
        server.verify().await;
    }

    #[tokio::test]
    async fn keycloak_refreshes_when_near_expiry() {
        let server = MockServer::start().await;

        // Both calls return immediately-expired tokens (expires_in: 0).
        Mock::given(method("POST"))
            .and(path("/auth/realms/test-realm/protocol/openid-connect/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "first",
                    "expires_in": 0
                })),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/auth/realms/test-realm/protocol/openid-connect/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "second",
                    "expires_in": 0
                })),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let creds = KeycloakCreds {
            url: server.uri(),
            realm: "test-realm".to_string(),
            client_id: "test-client".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
        };
        let r = Refresher::Keycloak {
            client: Client::new(),
            creds,
            state: tokio::sync::Mutex::new(TokenState::expired()),
        };

        // First call: expired initial state → fetches → returns "first".
        let t1 = r.token().await.unwrap();
        assert_eq!(t1, "first");

        // Second call: expires_in=0 means already expired → re-fetches → returns "second".
        let t2 = r.token().await.unwrap();
        assert_eq!(t2, "second");

        // Verify two POSTs were made.
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 2, "expected 2 Keycloak POSTs, got {}", received.len());
    }
}
