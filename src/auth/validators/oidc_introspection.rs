use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use super::common::{RealmAccess, collect_roles, extract_issuer, oidc_issuer_of};
use crate::{
    auth::validator::{Principal, ValidationError},
    config::{KeycloakConfig, PartyCredentials},
};

/// How long an introspection result stays cached. Short enough that revocation
/// on the IdP takes effect quickly; long enough that UI bursts (dozens of
/// parallel calls on page load) collapse to one round trip per token.
const CACHE_TTL: Duration = Duration::from_secs(30);

/// How long OIDC discovery documents stay cached.
const DISCOVERY_TTL: Duration = Duration::from_secs(3600);

/// RFC 7662 token introspection validator.
///
/// Provider-agnostic: works against any OIDC IdP that implements introspection
/// (Keycloak, Auth0, Okta, Cognito, Google, ...). Trusted issuers are derived
/// from the top-level inbound auth config plus any `party_credentials` rows —
/// no separate config file.
pub struct OidcIntrospectionValidator {
    /// Inbound auth config (top-level `config.keycloak`, what the frontend
    /// logs in against).
    inbound: Option<KeycloakConfig>,
    /// Outbound party credentials. Tokens issued by any of these issuers are
    /// also accepted — lets operators who already log in against a party's
    /// IdP reuse the same identity for node operation.
    party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    /// Short-lived cache keyed by sha256(token).
    token_cache: RwLock<HashMap<[u8; 32], CachedToken>>,
    /// Discovery endpoint cache keyed by issuer.
    discovery_cache: RwLock<HashMap<String, CachedDiscovery>>,
    http: reqwest::Client,
}

struct CachedToken {
    principal: Principal,
    expires_at: SystemTime,
}

struct CachedDiscovery {
    introspection_endpoint: String,
    expires_at: SystemTime,
}

#[derive(Deserialize)]
struct OidcDiscovery {
    introspection_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct IntrospectionResponse {
    active: bool,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    email: Option<String>,
    /// Keycloak nests roles here.
    #[serde(default)]
    realm_access: Option<RealmAccess>,
    /// Space-separated scopes, occasionally used as a roles carrier.
    #[serde(default)]
    scope: Option<String>,
    /// Flat roles array — some providers put roles directly under this key.
    #[serde(default)]
    roles: Option<Vec<String>>,
}

impl OidcIntrospectionValidator {
    /// Build an introspection validator that trusts tokens from the top-level
    /// inbound IdP config and any issuer present in `party_credentials`.
    pub fn new(
        inbound: Option<KeycloakConfig>,
        party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            inbound,
            party_credentials,
            token_cache: RwLock::new(HashMap::new()),
            discovery_cache: RwLock::new(HashMap::new()),
            http,
        }
    }

    /// Validate a bearer token via RFC 7662 introspection against the IdP
    /// that issued it.
    ///
    /// # Errors
    ///
    /// Returns `ValidationError::MissingToken` if the token is empty,
    /// `::MalformedToken` if the JWT shape is invalid, `::UntrustedIssuer`
    /// if no configured IdP matches the `iss` claim, `::DiscoveryFailed` or
    /// `::IntrospectionFailed` for network/IdP errors, or `::InactiveToken`
    /// when the IdP reports the token as inactive.
    pub async fn validate(&self, token: &str) -> Result<Principal, ValidationError> {
        if token.is_empty() {
            return Err(ValidationError::MissingToken);
        }

        let key = token_cache_key(token);
        if let Some(principal) = self.cache_get(&key).await {
            return Ok(principal);
        }

        let issuer = extract_issuer(token)?;
        let config = self.find_trusted_config(&issuer).await.ok_or_else(|| {
            tracing::warn!("rejected token from untrusted issuer: {issuer}");
            ValidationError::UntrustedIssuer(issuer.clone())
        })?;

        let endpoint = self.resolve_introspection_endpoint(&issuer).await?;
        let response = self.introspect(&endpoint, token, &config).await?;

        if !response.active {
            return Err(ValidationError::InactiveToken);
        }

        let roles = collect_roles(
            response.realm_access.as_ref(),
            response.roles.as_deref(),
            response.scope.as_deref(),
        );
        let principal = Principal {
            sub: response.sub.unwrap_or_default(),
            issuer,
            roles,
            email: response.email,
        };

        self.cache_put(key, principal.clone()).await;
        Ok(principal)
    }

    async fn cache_get(&self, key: &[u8; 32]) -> Option<Principal> {
        let cache = self.token_cache.read().await;
        let entry = cache.get(key)?;
        if entry.expires_at > SystemTime::now() {
            Some(entry.principal.clone())
        } else {
            None
        }
    }

    async fn cache_put(&self, key: [u8; 32], principal: Principal) {
        let mut cache = self.token_cache.write().await;
        // Opportunistic eviction: drop expired entries when we write, so the
        // map doesn't grow without bound under token churn. Not an LRU but
        // good enough for the typical handful-of-users workload.
        let now = SystemTime::now();
        cache.retain(|_, v| v.expires_at > now);
        cache.insert(
            key,
            CachedToken {
                principal,
                expires_at: now + CACHE_TTL,
            },
        );
    }

    /// Find a `KeycloakConfig` whose OIDC issuer matches `issuer`. Checks the
    /// top-level inbound config first, then party credentials.
    async fn find_trusted_config(&self, issuer: &str) -> Option<KeycloakConfig> {
        if let Some(ref cfg) = self.inbound
            && oidc_issuer_of(cfg) == issuer
        {
            return Some(cfg.clone());
        }
        let creds = self.party_credentials.read().await;
        creds
            .iter()
            .find(|p| oidc_issuer_of(&p.keycloak) == issuer)
            .map(|p| p.keycloak.clone())
    }

    async fn resolve_introspection_endpoint(
        &self,
        issuer: &str,
    ) -> Result<String, ValidationError> {
        {
            let cache = self.discovery_cache.read().await;
            if let Some(entry) = cache.get(issuer)
                && entry.expires_at > SystemTime::now()
            {
                return Ok(entry.introspection_endpoint.clone());
            }
        }

        let url = format!("{issuer}/.well-known/openid-configuration");
        let doc: OidcDiscovery = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| ValidationError::DiscoveryFailed {
                issuer: issuer.to_string(),
                message: e.to_string(),
            })?
            .error_for_status()
            .map_err(|e| ValidationError::DiscoveryFailed {
                issuer: issuer.to_string(),
                message: e.to_string(),
            })?
            .json()
            .await
            .map_err(|e| ValidationError::DiscoveryFailed {
                issuer: issuer.to_string(),
                message: e.to_string(),
            })?;

        let endpoint =
            doc.introspection_endpoint
                .ok_or_else(|| ValidationError::DiscoveryFailed {
                    issuer: issuer.to_string(),
                    message: "provider does not advertise introspection_endpoint".to_string(),
                })?;

        let mut cache = self.discovery_cache.write().await;
        cache.insert(
            issuer.to_string(),
            CachedDiscovery {
                introspection_endpoint: endpoint.clone(),
                expires_at: SystemTime::now() + DISCOVERY_TTL,
            },
        );
        Ok(endpoint)
    }

    async fn introspect(
        &self,
        endpoint: &str,
        token: &str,
        config: &KeycloakConfig,
    ) -> Result<IntrospectionResponse, ValidationError> {
        let mut form = vec![("token", token.to_string())];
        form.push(("token_type_hint", "access_token".to_string()));

        let mut request = self.http.post(endpoint);
        // RFC 7662 §2.2: introspecting client authenticates. Confidential
        // clients use Basic auth; password-flow clients don't have a usable
        // secret here so the request will fail with a clear IdP error.
        if let Some(ref secret) = config.client_secret {
            request = request.basic_auth(&config.client_id, Some(secret));
        } else {
            form.push(("client_id", config.client_id.clone()));
        }

        let resp = request
            .form(&form)
            .send()
            .await
            .map_err(|e| ValidationError::IntrospectionFailed(e.to_string()))?
            .error_for_status()
            .map_err(|e| ValidationError::IntrospectionFailed(e.to_string()))?;

        resp.json::<IntrospectionResponse>()
            .await
            .map_err(|e| ValidationError::IntrospectionFailed(e.to_string()))
    }
}

fn token_cache_key(token: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::error::Result;

    /// Build an unsigned JWT-shaped token carrying just the `iss` claim. The
    /// introspection validator only reads `iss` locally (to route to the right
    /// IdP); the IdP's introspection response is authoritative, so the
    /// signature segment is irrelevant here.
    fn token_with_issuer(issuer: &str) -> String {
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"iss":"{issuer}","sub":"alice"}}"#));
        format!("header.{payload}.sig")
    }

    fn inbound_config(server_uri: &str) -> KeycloakConfig {
        KeycloakConfig {
            url: server_uri.to_string(),
            realm: "test".to_string(),
            client_id: "dpm".to_string(),
            client_secret: Some("secret".to_string()),
            username: None,
            password: None,
        }
    }

    fn validator(inbound: KeycloakConfig) -> OidcIntrospectionValidator {
        OidcIntrospectionValidator::new(
            Some(inbound),
            Arc::new(RwLock::new(Vec::new())),
            reqwest::Client::new(),
        )
    }

    #[tokio::test]
    async fn accepts_active_token_and_projects_roles() -> Result {
        let server = MockServer::start().await;
        let issuer = format!("{}/realms/test", server.uri());

        Mock::given(method("GET"))
            .and(path("/realms/test/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "introspection_endpoint": format!("{}/introspect", server.uri()),
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "sub": "alice",
                "realm_access": { "roles": ["admin", "user"] },
            })))
            .mount(&server)
            .await;

        let token = token_with_issuer(&issuer);
        let principal = validator(inbound_config(&server.uri()))
            .validate(&token)
            .await
            .map_err(|e| anyhow::anyhow!("expected active token to validate: {e:?}"))?;

        assert_eq!(principal.sub, "alice");
        assert_eq!(principal.issuer, issuer);
        assert!(principal.has_role("admin"));
        assert!(principal.has_role("user"));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_inactive_token() {
        let server = MockServer::start().await;
        let issuer = format!("{}/realms/test", server.uri());

        Mock::given(method("GET"))
            .and(path("/realms/test/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "introspection_endpoint": format!("{}/introspect", server.uri()),
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "active": false })),
            )
            .mount(&server)
            .await;

        let token = token_with_issuer(&issuer);
        let result = validator(inbound_config(&server.uri()))
            .validate(&token)
            .await;
        assert!(matches!(result, Err(ValidationError::InactiveToken)));
    }

    #[tokio::test]
    async fn rejects_untrusted_issuer_without_calling_idp() {
        // The issuer in the token matches no configured IdP, so validation
        // fails before any discovery/introspection call. The mock server has
        // no mounts — any outbound call would 404 and surface as a different
        // error, so reaching UntrustedIssuer proves the short-circuit.
        let server = MockServer::start().await;
        let token = token_with_issuer("https://evil.example/realms/attacker");
        let result = validator(inbound_config(&server.uri()))
            .validate(&token)
            .await;
        assert!(matches!(result, Err(ValidationError::UntrustedIssuer(_))));
    }
}
