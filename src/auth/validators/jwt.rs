//! JWT signature-verification validator.
//!
//! Alternative to `OidcIntrospectionValidator`: instead of calling the IdP's
//! introspection endpoint per request (which requires a confidential client +
//! `client_secret` and a server-to-server round trip), we verify the token's
//! RS256 signature locally against the realm's published JWKS. No outbound
//! call on the hot path once the JWKS is cached, and no IdP-side client
//! permission needed.
//!
//! Trusted issuers come from the same source as the introspection validator:
//! the top-level inbound `KeycloakConfig` plus any `party_credentials` rows.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use base64::Engine;
use jsonwebtoken::{DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::{
    auth::validator::{Principal, ValidationError},
    config::{KeycloakConfig, PartyCredentials},
};

/// How long JWKS documents stay cached. Keycloak rotates signing keys
/// infrequently; an hour amortizes the discovery + JWKS fetch over many
/// requests while still picking up rotations within a reasonable window.
const JWKS_TTL: Duration = Duration::from_secs(3600);

pub struct JwtValidator {
    inbound: Option<KeycloakConfig>,
    party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    /// JWKS cache keyed by issuer.
    jwks_cache: RwLock<HashMap<String, CachedJwks>>,
    http: reqwest::Client,
}

struct CachedJwks {
    keys: HashMap<String, DecodingKey>,
    expires_at: SystemTime,
}

#[derive(Deserialize)]
struct OidcDiscovery {
    jwks_uri: String,
}

#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

/// Loose JWK shape. The `jsonwebtoken` crate ships a strict `Jwk` type that
/// rejects Keycloak's published keys (encryption-mode entries, extra fields,
/// algorithm variants the crate's untagged enum cannot resolve). Parsing the
/// raw fields ourselves and feeding the underlying RSA / EC components to
/// `DecodingKey` directly avoids that brittleness.
#[derive(Deserialize)]
struct Jwk {
    kid: Option<String>,
    #[serde(default)]
    kty: Option<String>,
    #[serde(rename = "use", default)]
    use_: Option<String>,
    /// RSA modulus (base64url). Present for `kty: "RSA"`.
    #[serde(default)]
    n: Option<String>,
    /// RSA public exponent (base64url). Present for `kty: "RSA"`.
    #[serde(default)]
    e: Option<String>,
    /// EC curve name (e.g. "P-256"). Present for `kty: "EC"`.
    #[serde(default)]
    crv: Option<String>,
    /// EC x coordinate (base64url). Present for `kty: "EC"`.
    #[serde(default)]
    x: Option<String>,
    /// EC y coordinate (base64url). Present for `kty: "EC"`.
    #[serde(default)]
    y: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    realm_access: Option<RealmAccess>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    roles: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RealmAccess {
    #[serde(default)]
    roles: Vec<String>,
}

impl JwtValidator {
    pub fn new(
        inbound: Option<KeycloakConfig>,
        party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    ) -> Self {
        Self {
            inbound,
            party_credentials,
            jwks_cache: RwLock::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client build"),
        }
    }

    /// Verify the token's signature and standard claims, returning the
    /// `Principal` carried in its payload.
    ///
    /// # Errors
    ///
    /// Returns `ValidationError::MissingToken` for empty input,
    /// `::MalformedToken` for shape problems, `::UntrustedIssuer` if the `iss`
    /// claim does not match a configured IdP, `::DiscoveryFailed` for OIDC
    /// metadata or JWKS fetch failures, or `::InactiveToken` for an invalid
    /// signature, expired token, or wrong algorithm.
    pub async fn validate(&self, token: &str) -> Result<Principal, ValidationError> {
        if token.is_empty() {
            return Err(ValidationError::MissingToken);
        }

        let issuer = extract_issuer(token)?;
        if !self.is_trusted_issuer(&issuer).await {
            tracing::warn!("rejected token from untrusted issuer: {issuer}");
            return Err(ValidationError::UntrustedIssuer(issuer));
        }

        let header = decode_header(token).map_err(|_| ValidationError::MalformedToken)?;
        let kid = header.kid.ok_or(ValidationError::MalformedToken)?;

        let key = self.resolve_key(&issuer, &kid).await?;

        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&issuer]);
        // Audience varies per Keycloak client; verify it at the role-check
        // layer instead of the signature layer to keep this validator usable
        // across multiple frontend clients.
        validation.validate_aud = false;
        // `leeway` defaults to 60s which is fine.

        let data = decode::<Claims>(token, &key, &validation).map_err(|e| {
            tracing::warn!("jwt verify failed: {e}");
            ValidationError::InactiveToken
        })?;

        let claims = data.claims;
        let roles = collect_roles(&claims);
        Ok(Principal {
            sub: claims.sub.unwrap_or_default(),
            issuer,
            roles,
            email: claims.email,
        })
    }

    async fn is_trusted_issuer(&self, issuer: &str) -> bool {
        if let Some(ref cfg) = self.inbound
            && oidc_issuer_of(cfg) == issuer
        {
            return true;
        }
        let creds = self.party_credentials.read().await;
        creds.iter().any(|p| oidc_issuer_of(&p.keycloak) == issuer)
    }

    async fn resolve_key(&self, issuer: &str, kid: &str) -> Result<DecodingKey, ValidationError> {
        // Fast path: cached JWKS still fresh.
        {
            let cache = self.jwks_cache.read().await;
            if let Some(entry) = cache.get(issuer)
                && entry.expires_at > SystemTime::now()
                && let Some(key) = entry.keys.get(kid)
            {
                return Ok(key.clone());
            }
        }

        let keys = self.fetch_jwks(issuer).await?;
        let key = keys
            .get(kid)
            .cloned()
            .ok_or_else(|| ValidationError::DiscoveryFailed {
                issuer: issuer.to_string(),
                message: format!("kid {kid} not present in JWKS"),
            })?;

        let mut cache = self.jwks_cache.write().await;
        cache.insert(
            issuer.to_string(),
            CachedJwks {
                keys,
                expires_at: SystemTime::now() + JWKS_TTL,
            },
        );
        Ok(key)
    }

    async fn fetch_jwks(
        &self,
        issuer: &str,
    ) -> Result<HashMap<String, DecodingKey>, ValidationError> {
        let discovery_url = format!("{issuer}/.well-known/openid-configuration");
        let discovery: OidcDiscovery = self
            .http
            .get(&discovery_url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
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

        let jwks: JwkSet = self
            .http
            .get(&discovery.jwks_uri)
            .send()
            .await
            .and_then(|r| r.error_for_status())
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

        let mut keys = HashMap::new();
        for jwk in jwks.keys {
            let Some(kid) = jwk.kid.clone() else { continue };
            // Skip explicitly non-signature keys (Keycloak publishes encryption
            // keys in the same JWKS).
            if let Some(ref u) = jwk.use_
                && u != "sig"
            {
                continue;
            }
            match decoding_key_from_jwk(&jwk) {
                Ok(key) => {
                    keys.insert(kid, key);
                }
                Err(e) => {
                    tracing::warn!("skipping JWK kid={kid}: {e}");
                }
            }
        }
        Ok(keys)
    }
}

fn oidc_issuer_of(cfg: &KeycloakConfig) -> String {
    format!("{}/realms/{}", cfg.url.trim_end_matches('/'), cfg.realm)
}

fn extract_issuer(token: &str) -> Result<String, ValidationError> {
    let mut parts = token.split('.');
    let _header = parts.next().ok_or(ValidationError::MalformedToken)?;
    let payload = parts.next().ok_or(ValidationError::MalformedToken)?;

    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| {
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

fn collect_roles(claims: &Claims) -> Vec<String> {
    let mut roles = Vec::new();
    if let Some(ref r) = claims.realm_access {
        roles.extend(r.roles.iter().cloned());
    }
    if let Some(ref r) = claims.roles {
        for role in r {
            if !roles.contains(role) {
                roles.push(role.clone());
            }
        }
    }
    if let Some(ref scope) = claims.scope {
        for s in scope.split_whitespace() {
            let s = s.to_string();
            if !roles.contains(&s) {
                roles.push(s);
            }
        }
    }
    roles
}

/// Build a `DecodingKey` from a parsed JWK. Goes through the raw
/// components (RSA `n`/`e`, EC `x`/`y`/`crv`) rather than the upstream
/// `DecodingKey::from_jwk` because the latter's strict deserializer rejects
/// some valid Keycloak entries (e.g. those whose `alg` field carries a
/// value the crate's `AlgorithmParameters` untagged enum cannot resolve).
fn decoding_key_from_jwk(jwk: &Jwk) -> Result<DecodingKey, String> {
    let kty = jwk.kty.as_deref().ok_or("missing kty")?;
    match kty {
        "RSA" => {
            let n = jwk.n.as_deref().ok_or("RSA JWK missing n")?;
            let e = jwk.e.as_deref().ok_or("RSA JWK missing e")?;
            DecodingKey::from_rsa_components(n, e).map_err(|err| err.to_string())
        }
        "EC" => {
            let x = jwk.x.as_deref().ok_or("EC JWK missing x")?;
            let y = jwk.y.as_deref().ok_or("EC JWK missing y")?;
            // `crv` is informational here; jsonwebtoken's EC validator pulls
            // the curve from the algorithm specified in the token's header.
            let _ = jwk.crv.as_deref();
            DecodingKey::from_ec_components(x, y).map_err(|err| err.to_string())
        }
        other => Err(format!("unsupported kty: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_jwt(payload: &serde_json::Value) -> String {
        let header =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"RS256\"}");
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(payload).unwrap());
        format!("{header}.{body}.sig")
    }

    #[test]
    fn extract_issuer_strips_trailing_slash() {
        let token = make_jwt(&serde_json::json!({ "iss": "https://kc.example/realms/r/" }));
        assert_eq!(
            extract_issuer(&token).unwrap(),
            "https://kc.example/realms/r"
        );
    }

    #[test]
    fn extract_issuer_handles_missing_iss() {
        let token = make_jwt(&serde_json::json!({ "sub": "x" }));
        assert!(matches!(
            extract_issuer(&token),
            Err(ValidationError::MalformedToken)
        ));
    }

    #[test]
    fn collect_roles_merges_realm_scope_and_roles() {
        let claims = Claims {
            sub: None,
            email: None,
            realm_access: Some(RealmAccess {
                roles: vec!["admin".into(), "user".into()],
            }),
            roles: Some(vec!["user".into(), "viewer".into()]),
            scope: Some("openid email viewer".into()),
        };
        let roles = collect_roles(&claims);
        assert_eq!(roles, vec!["admin", "user", "viewer", "openid", "email"]);
    }
}
