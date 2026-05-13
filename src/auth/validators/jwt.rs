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

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::RwLock;

use super::common::{
    RealmAccess, auth0_issuer_of, collect_roles, extract_issuer, oidc_issuer_of,
};
use crate::{
    auth::validator::{Principal, ValidationError},
    config::{Auth0Config, KeycloakConfig, PartyCredentials},
};

/// How long JWKS documents stay cached. Keycloak rotates signing keys
/// infrequently; an hour amortizes the discovery + JWKS fetch over many
/// requests while still picking up rotations within a reasonable window.
const JWKS_TTL: Duration = Duration::from_secs(3600);

pub struct JwtValidator {
    inbound: Option<KeycloakConfig>,
    auth0: Option<Auth0Config>,
    party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    /// JWKS cache keyed by issuer.
    jwks_cache: RwLock<HashMap<String, CachedJwks>>,
    http: reqwest::Client,
}

struct CachedJwks {
    keys: HashMap<String, KeyEntry>,
    expires_at: SystemTime,
}

#[derive(Clone)]
struct KeyEntry {
    key: DecodingKey,
    /// Algorithm pinned at JWKS-fetch time. Either taken from the JWK's
    /// own `alg` field or derived from `kty`. Used to verify the signature
    /// and to reject any token whose header advertises a different alg —
    /// closes JWT alg-confusion attacks (e.g. switching RS256 → HS256).
    alg: Algorithm,
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
    /// Signing algorithm advertised by the IdP. When present, pinned and
    /// cross-checked against the token header's alg.
    #[serde(default)]
    alg: Option<String>,
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
    /// OIDC "authorized party" — the client_id this token was issued to.
    /// We require it to equal the matched `KeycloakConfig.client_id` so a
    /// token issued to a different client in the same realm cannot be
    /// reused against this service even if the realm-level role set matches.
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    realm_access: Option<RealmAccess>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    roles: Option<Vec<String>>,
}

impl JwtValidator {
    pub fn new(
        inbound: Option<KeycloakConfig>,
        auth0: Option<Auth0Config>,
        party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            inbound,
            auth0,
            party_credentials,
            jwks_cache: RwLock::new(HashMap::new()),
            http,
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
    /// signature, expired token, wrong algorithm, or `azp` that doesn't
    /// match the matched config's `client_id`.
    pub async fn validate(&self, token: &str) -> Result<Principal, ValidationError> {
        if token.is_empty() {
            return Err(ValidationError::MissingToken);
        }

        let issuer = extract_issuer(token)?;
        let Some(expected_client_id) = self.find_trusted_client_id(&issuer).await else {
            tracing::warn!("rejected token from untrusted issuer: {issuer}");
            return Err(ValidationError::UntrustedIssuer(issuer));
        };

        let header = decode_header(token).map_err(|_| ValidationError::MalformedToken)?;
        let kid = header.kid.ok_or(ValidationError::MalformedToken)?;

        let entry = self.resolve_key(&issuer, &kid).await?;

        // Pin algorithm: reject if the token's header advertises an alg
        // different from the JWK's. Without this, an attacker who could
        // forge a token with `alg: HS256` would have it verified against
        // an HMAC key derived from the public modulus — classic JWT
        // alg-confusion. The jsonwebtoken crate cross-checks key-type
        // versus alg internally, so this is defense in depth, but the
        // explicit check keeps the invariant local.
        if header.alg != entry.alg {
            tracing::warn!(
                "jwt header alg {:?} does not match JWK alg {:?}",
                header.alg,
                entry.alg
            );
            return Err(ValidationError::InactiveToken);
        }

        let mut validation = Validation::new(entry.alg);
        // Accept both `iss` shapes: Keycloak emits no trailing slash, Auth0
        // emits one. `extract_issuer` normalises by stripping, so add the
        // slashed variant alongside for the strict in-crate check.
        let issuer_with_slash = format!("{issuer}/");
        validation.set_issuer(&[issuer.as_str(), issuer_with_slash.as_str()]);
        // Audience is intentionally not enforced — Keycloak's `aud` claim
        // varies per realm config and often points to a sibling service
        // (e.g. the wallet API) rather than this service. Cross-client
        // privilege escalation is closed below by requiring `azp` (the
        // OIDC "authorized party") to equal this validator's matched
        // client_id, which Keycloak fixes to the client that obtained
        // the token.
        validation.validate_aud = false;
        // `leeway` defaults to 60s which is fine.

        let data = decode::<Claims>(token, &entry.key, &validation).map_err(|e| {
            tracing::warn!("jwt verify failed: {e}");
            ValidationError::InactiveToken
        })?;

        let claims = data.claims;

        // Enforce azp == matched config's client_id.
        match claims.azp.as_deref() {
            Some(azp) if azp == expected_client_id => {}
            other => {
                tracing::warn!(
                    "jwt azp {:?} does not match expected client_id {}",
                    other,
                    expected_client_id
                );
                return Err(ValidationError::InactiveToken);
            }
        }

        let roles = collect_roles(
            claims.realm_access.as_ref(),
            claims.roles.as_deref(),
            claims.scope.as_deref(),
        );
        Ok(Principal {
            sub: claims.sub.unwrap_or_default(),
            issuer,
            roles,
            email: claims.email,
        })
    }

    /// Find the expected `client_id` (for the `azp` check) corresponding to
    /// the given issuer. Searches inbound Keycloak, inbound Auth0, and any
    /// per-party Keycloak or Auth0 configs.
    async fn find_trusted_client_id(&self, issuer: &str) -> Option<String> {
        if let Some(ref cfg) = self.inbound
            && oidc_issuer_of(cfg) == issuer
        {
            return Some(cfg.client_id.clone());
        }
        if let Some(ref cfg) = self.auth0
            && auth0_issuer_of(cfg) == issuer
        {
            return Some(cfg.client_id.clone());
        }
        let creds = self.party_credentials.read().await;
        for party in creds.iter() {
            if let Some(ref a) = party.auth0
                && format!("https://{}", a.domain.trim_end_matches('/')) == issuer
            {
                return Some(a.client_id.clone());
            }
            if oidc_issuer_of(&party.keycloak) == issuer {
                return Some(party.keycloak.client_id.clone());
            }
        }
        None
    }

    async fn resolve_key(&self, issuer: &str, kid: &str) -> Result<KeyEntry, ValidationError> {
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
        let entry = keys
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
        Ok(entry)
    }

    async fn fetch_jwks(&self, issuer: &str) -> Result<HashMap<String, KeyEntry>, ValidationError> {
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
            let alg = match resolve_algorithm(&jwk) {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("skipping JWK kid={kid}: {e}");
                    continue;
                }
            };
            match decoding_key_from_jwk(&jwk) {
                Ok(key) => {
                    keys.insert(kid, KeyEntry { key, alg });
                }
                Err(e) => {
                    tracing::warn!("skipping JWK kid={kid}: {e}");
                }
            }
        }
        Ok(keys)
    }
}

/// Determine which `Algorithm` to verify with for a given JWK.
///
/// Prefers the JWK's own `alg` field when present; falls back to a
/// `kty`-based default (RSA → RS256, EC → ES256). Restricts to a small
/// asymmetric allowlist; symmetric algorithms (HS256/384/512) and `none`
/// are rejected because we never want to verify a token with them
/// against a key fetched from a public JWKS.
///
/// # Errors
///
/// Returns an error string if the JWK declares a disallowed `alg`
/// (symmetric, `none`, or otherwise unrecognised), or if both `alg` and
/// a recognisable `kty` default are missing.
fn resolve_algorithm(jwk: &Jwk) -> Result<Algorithm, String> {
    if let Some(alg_str) = jwk.alg.as_deref() {
        return match alg_str {
            "RS256" => Ok(Algorithm::RS256),
            "RS384" => Ok(Algorithm::RS384),
            "RS512" => Ok(Algorithm::RS512),
            "ES256" => Ok(Algorithm::ES256),
            "ES384" => Ok(Algorithm::ES384),
            "PS256" => Ok(Algorithm::PS256),
            "PS384" => Ok(Algorithm::PS384),
            "PS512" => Ok(Algorithm::PS512),
            "EdDSA" => Ok(Algorithm::EdDSA),
            other => Err(format!("disallowed JWK alg: {other}")),
        };
    }
    match jwk.kty.as_deref() {
        Some("RSA") => Ok(Algorithm::RS256),
        Some("EC") => Ok(Algorithm::ES256),
        _ => Err("JWK has no alg and no resolvable kty default".to_string()),
    }
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

    fn jwk(alg: Option<&str>, kty: Option<&str>) -> Jwk {
        Jwk {
            kid: Some("k".into()),
            kty: kty.map(str::to_string),
            use_: None,
            alg: alg.map(str::to_string),
            n: None,
            e: None,
            crv: None,
            x: None,
            y: None,
        }
    }

    #[test]
    fn resolve_algorithm_prefers_jwk_alg() {
        assert_eq!(
            resolve_algorithm(&jwk(Some("RS256"), Some("RSA"))).unwrap(),
            Algorithm::RS256
        );
        assert_eq!(
            resolve_algorithm(&jwk(Some("ES256"), Some("EC"))).unwrap(),
            Algorithm::ES256
        );
    }

    #[test]
    fn resolve_algorithm_rejects_symmetric_and_none() {
        // Symmetric and `none` would let an attacker forge tokens against
        // a key fetched from a public JWKS — never honour them.
        for bad in ["HS256", "HS384", "HS512", "none", "NoNe"] {
            assert!(resolve_algorithm(&jwk(Some(bad), Some("RSA"))).is_err());
        }
    }

    #[test]
    fn resolve_algorithm_falls_back_to_kty_default() {
        assert_eq!(
            resolve_algorithm(&jwk(None, Some("RSA"))).unwrap(),
            Algorithm::RS256
        );
        assert_eq!(
            resolve_algorithm(&jwk(None, Some("EC"))).unwrap(),
            Algorithm::ES256
        );
        assert!(resolve_algorithm(&jwk(None, None)).is_err());
    }
}
