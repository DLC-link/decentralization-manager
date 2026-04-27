use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

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

#[derive(Deserialize)]
struct RealmAccess {
    #[serde(default)]
    roles: Vec<String>,
}

impl OidcIntrospectionValidator {
    /// Build an introspection validator that trusts tokens from the top-level
    /// inbound IdP config and any issuer present in `party_credentials`.
    pub fn new(
        inbound: Option<KeycloakConfig>,
        party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    ) -> Self {
        Self {
            inbound,
            party_credentials,
            token_cache: RwLock::new(HashMap::new()),
            discovery_cache: RwLock::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client build"),
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

        let roles = collect_roles(&response);
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

/// Canonical OIDC issuer for a KeycloakConfig-shaped entry. Keycloak issues
/// `{url}/realms/{realm}`. Other providers use different conventions but this
/// struct is Keycloak-shaped today; non-Keycloak support plugs in by adding a
/// variant and adjusting this function.
fn oidc_issuer_of(cfg: &KeycloakConfig) -> String {
    format!("{}/realms/{}", cfg.url.trim_end_matches('/'), cfg.realm)
}

/// Extract the `iss` claim from a JWT without verifying its signature. Used
/// only to route to the correct trusted config; the introspection call is the
/// authoritative check.
fn extract_issuer(token: &str) -> Result<String, ValidationError> {
    let mut parts = token.split('.');
    let _header = parts.next().ok_or(ValidationError::MalformedToken)?;
    let payload = parts.next().ok_or(ValidationError::MalformedToken)?;

    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| {
            // Some providers use standard (padded) base64.
            let padding = (4 - payload.len() % 4) % 4;
            let padded = format!("{payload}{}", "=".repeat(padding));
            base64::engine::general_purpose::STANDARD.decode(padded)
        })
        .map_err(|_| ValidationError::MalformedToken)?;

    let claims: serde_json::Value =
        serde_json::from_slice(&decoded).map_err(|_| ValidationError::MalformedToken)?;

    claims
        .get("iss")
        .and_then(|v| v.as_str())
        .map(|s| s.trim_end_matches('/').to_string())
        .ok_or(ValidationError::MalformedToken)
}

fn collect_roles(resp: &IntrospectionResponse) -> Vec<String> {
    let mut roles = Vec::new();
    if let Some(ref r) = resp.realm_access {
        roles.extend(r.roles.iter().cloned());
    }
    if let Some(ref r) = resp.roles {
        for role in r {
            if !roles.contains(role) {
                roles.push(role.clone());
            }
        }
    }
    if let Some(ref scope) = resp.scope {
        for s in scope.split_whitespace() {
            let s = s.to_string();
            if !roles.contains(&s) {
                roles.push(s);
            }
        }
    }
    roles
}

fn token_cache_key(token: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issuer_extraction_handles_url_safe_base64() {
        // {"iss":"https://keycloak.example.com/realms/foo","sub":"alice"}
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"iss":"https://keycloak.example.com/realms/foo","sub":"alice"}"#);
        let token = format!("header.{payload}.sig");
        assert_eq!(
            extract_issuer(&token).unwrap(),
            "https://keycloak.example.com/realms/foo"
        );
    }

    #[test]
    fn issuer_extraction_strips_trailing_slash() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"iss":"https://example.com/"}"#);
        let token = format!("header.{payload}.sig");
        assert_eq!(extract_issuer(&token).unwrap(), "https://example.com");
    }

    #[test]
    fn malformed_token_rejected() {
        assert!(matches!(
            extract_issuer("not-a-jwt"),
            Err(ValidationError::MalformedToken)
        ));
    }

    #[test]
    fn oidc_issuer_matches_keycloak_shape() {
        let cfg = KeycloakConfig {
            url: "https://keycloak.example.com/".to_string(),
            realm: "bitsafe".to_string(),
            client_id: "dpm".to_string(),
            client_secret: None,
            username: None,
            password: None,
        };
        assert_eq!(
            oidc_issuer_of(&cfg),
            "https://keycloak.example.com/realms/bitsafe"
        );
    }
}
