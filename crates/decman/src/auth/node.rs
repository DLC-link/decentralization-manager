//! This node's own Ledger API login. The node acts as its node party with
//! it, apart from the credentials of any decentralized party.

use std::sync::Arc;

use super::{AuthError, TokenManager};
use crate::{canton_id::CantonId, config::NodeIdentityRecord};

/// Where the node login's tokens come from.
#[derive(Clone)]
pub enum NodeTokenSource {
    /// Client credentials at the stored IdP. The manager renews the token
    /// when it expires.
    Idp(Arc<TokenManager>),
    /// The unsafe token of insecure mode.
    Mock(String),
}

/// This node's identity with a login that works.
#[derive(Clone)]
pub struct NodeIdentity {
    pub node_party: CantonId,
    /// The participant the node party was set up on.
    pub participant_id: CantonId,
    /// The Ledger API user the login authenticates as.
    pub user_id: String,
    pub tokens: NodeTokenSource,
}

impl NodeIdentity {
    /// Log in with the IdP client of `record`.
    ///
    /// # Errors
    ///
    /// Returns an error when `record` holds no login or the IdP refuses it.
    pub async fn connect(record: &NodeIdentityRecord) -> Result<Self, AuthError> {
        let login = record
            .login
            .clone()
            .ok_or_else(|| AuthError::NoCredentials(record.node_party_id.to_string()))?;
        let manager =
            TokenManager::for_client(login, record.user_id.clone(), record.node_party_id.clone())
                .await?;
        Ok(Self::with_tokens(
            record,
            NodeTokenSource::Idp(Arc::new(manager)),
        ))
    }

    /// `record` with the unsafe token of insecure mode.
    pub fn mock(record: &NodeIdentityRecord, token: String) -> Self {
        Self::with_tokens(record, NodeTokenSource::Mock(token))
    }

    fn with_tokens(record: &NodeIdentityRecord, tokens: NodeTokenSource) -> Self {
        Self {
            node_party: record.node_party_id.clone(),
            participant_id: record.participant_id.clone(),
            user_id: record.user_id.clone(),
            tokens,
        }
    }

    /// A token for the node login. An expired one is renewed first.
    ///
    /// # Errors
    ///
    /// Returns an error when the IdP refuses the renewal.
    pub async fn token(&self) -> Result<String, AuthError> {
        match &self.tokens {
            NodeTokenSource::Idp(manager) => manager.get_token().await,
            NodeTokenSource::Mock(token) => Ok(token.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, body_string, header, method, path},
    };

    use super::{NodeIdentity, NodeTokenSource};
    use crate::{
        auth::{AuthError, TokenManager, TokenSource},
        canton_id::CantonId,
        config::{Auth0M2MConfig, IdpClient, KeycloakConfig, NodeIdentityRecord},
    };

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
    const KEYCLOAK_TOKEN_PATH: &str = "/realms/canton/protocol/openid-connect/token";

    fn record(login: Option<IdpClient>) -> anyhow::Result<NodeIdentityRecord> {
        Ok(NodeIdentityRecord {
            node_party_id: CantonId::parse(&format!("decman-node-participant1::{NS}"))?,
            participant_id: CantonId::parse(&format!("participant1::{NS}"))?,
            user_id: "node-login".to_string(),
            login,
            created_at: 0,
            updated_at: 0,
        })
    }

    fn keycloak_login(url: String) -> IdpClient {
        IdpClient::Keycloak(KeycloakConfig {
            url,
            realm: "canton".to_string(),
            client_id: "node-client".to_string(),
            client_secret: Some("node-secret".to_string()),
            ..KeycloakConfig::default()
        })
    }

    /// The Keycloak login is a form-encoded `client_credentials` request at
    /// the realm's token endpoint. The mock answers only that exact form.
    #[tokio::test]
    async fn keycloak_login_mints_a_node_token() -> anyhow::Result<()> {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(KEYCLOAK_TOKEN_PATH))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string(
                "grant_type=client_credentials&client_id=node-client&client_secret=node-secret",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "node-token", "expires_in": 300})),
            )
            .expect(1)
            .mount(&idp)
            .await;

        let identity = NodeIdentity::connect(&record(Some(keycloak_login(idp.uri())))?).await?;

        assert_eq!(identity.user_id, "node-login");
        assert_eq!(identity.token().await?, "node-token");
        Ok(())
    }

    #[tokio::test]
    async fn a_refused_keycloak_login_fails_to_connect() -> anyhow::Result<()> {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(KEYCLOAK_TOKEN_PATH))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({"error": "unauthorized_client"})),
            )
            .mount(&idp)
            .await;

        let result = NodeIdentity::connect(&record(Some(keycloak_login(idp.uri())))?).await;

        assert!(
            matches!(result, Err(AuthError::M2MAuthFailed(_))),
            "a refused login connected"
        );
        Ok(())
    }

    /// The Auth0 login is JSON with the ledger audience. `new_auth0` always
    /// targets `https://<domain>`, so the manager starts here at the mock's
    /// plain-HTTP URL instead.
    #[tokio::test]
    async fn auth0_login_mints_a_node_token() -> anyhow::Result<()> {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(header("content-type", "application/json"))
            .and(body_json(json!({
                "grant_type": "client_credentials",
                "client_id": "node-m2m",
                "client_secret": "node-secret",
                "audience": "https://canton.network.global",
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "node-token", "expires_in": 300})),
            )
            .expect(1)
            .mount(&idp)
            .await;
        let record = record(None)?;
        let source = TokenSource::Auth0 {
            config: Auth0M2MConfig {
                domain: "tenant.us.auth0.com".to_string(),
                audience: "https://canton.network.global".to_string(),
                client_id: "node-m2m".to_string(),
                client_secret: "node-secret".to_string(),
            },
            token_url: format!("{base}/oauth/token", base = idp.uri()),
        };
        let manager =
            TokenManager::start(source, record.user_id.clone(), record.node_party_id.clone())
                .await?;

        let identity = NodeIdentity::with_tokens(&record, NodeTokenSource::Idp(Arc::new(manager)));

        assert_eq!(identity.token().await?, "node-token");
        Ok(())
    }

    #[tokio::test]
    async fn a_record_without_login_cannot_connect() -> anyhow::Result<()> {
        let result = NodeIdentity::connect(&record(None)?).await;

        assert!(matches!(result, Err(AuthError::NoCredentials(_))));
        Ok(())
    }

    #[tokio::test]
    async fn a_mock_identity_returns_the_unsafe_token() -> anyhow::Result<()> {
        let identity = NodeIdentity::mock(&record(None)?, "unsafe-token".to_string());

        assert_eq!(identity.token().await?, "unsafe-token");
        assert_eq!(identity.node_party.prefix, "decman-node-participant1");
        Ok(())
    }
}
