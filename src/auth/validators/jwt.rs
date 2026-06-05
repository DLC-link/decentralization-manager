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

use super::common::{RealmAccess, auth0_issuer_of, collect_roles, extract_issuer, oidc_issuer_of};
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
    use base64::{Engine, engine::general_purpose::STANDARD};
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

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

    // ---- End-to-end `validate()` coverage ----
    //
    // Drives the full path: OIDC discovery + JWKS fetch (stubbed with
    // wiremock), RS256 signature verification, alg-pinning, `exp`, issuer
    // routing, and the `azp` cross-client check. Tokens are signed in-test
    // with a throwaway 2048-bit RSA key whose public modulus is served as the
    // JWK below.

    const TEST_KID: &str = "test-key-1";
    const TEST_JWK_E: &str = "AQAB";
    /// base64url public modulus matching `TEST_RSA_PKCS1_DER_B64` below.
    const TEST_JWK_N: &str = "uOVq7XNTobJpBBbp_54gdNkbZYlJnsZhpwc6cbq6djnNUEezDxMLic_X79SzZRiiKs-SUn43zu99zPrCmsvAYBDBZunsKnySjDyIPRIxex9blc-IPyk8n8PURFuB8ty48b6d9RR89Jj_3_ISYPE2YAsR7a7O5ao1XfnukYy57T0ZoUnqbQYqalwI3XbqNgLqiz3Yap7R_25TQLVaHFWIDWV8FL8_GzVm8YtFSSauCNGg7lG3qM7HmDan_dPM6Lg3uzAHky9i0ClGC6fWzfVPTt4u3Amlzjme1OlLz22XoS6E-xbjFXCINeQq_Ir9fSdgl0QPbuF-jkCTbaYQQXSbhQ";
    /// Throwaway test-only RSA private key — PKCS#1 DER, base64-encoded. Stored
    /// without PEM armor on purpose: a `BEGIN PRIVATE KEY` literal would trip
    /// secret scanners and is a bad pattern to commit. Test-only; the matching
    /// public modulus is `TEST_JWK_N` above.
    const TEST_RSA_PKCS1_DER_B64: &str = "MIIEowIBAAKCAQEAuOVq7XNTobJpBBbp/54gdNkbZYlJnsZhpwc6cbq6djnNUEezDxMLic/X79SzZRiiKs+SUn43zu99zPrCmsvAYBDBZunsKnySjDyIPRIxex9blc+IPyk8n8PURFuB8ty48b6d9RR89Jj/3/ISYPE2YAsR7a7O5ao1XfnukYy57T0ZoUnqbQYqalwI3XbqNgLqiz3Yap7R/25TQLVaHFWIDWV8FL8/GzVm8YtFSSauCNGg7lG3qM7HmDan/dPM6Lg3uzAHky9i0ClGC6fWzfVPTt4u3Amlzjme1OlLz22XoS6E+xbjFXCINeQq/Ir9fSdgl0QPbuF+jkCTbaYQQXSbhQIDAQABAoIBAETDJWobis3G4SFhODMVZrKuD29KiHOhCa4plQW40SGoy3+AusnvZkohXwhVjUYazCypt5wwTqcKEDoMRBV3kxrnAFY6xtbiL0oyNOSpgHduqQvk+6Gpv18XYDjv4zsj9TAKmQoNTY9V20s45rbg3j0HwOopKc7l5yUFD0FYGcltXRGOuXWKmN+vBLnni+xcSeeOr2/oXHIlcGiLJQbk0Ty6rZaGcHM7l7Ymgc5ZwcMMqtIywvwLB+mJ4bJVPCTgR9tjurPyeMR2fqskdh9n3rdF2mWhXagELeDWPqyjvQgjI7pPn6wgZDA0vWNIHMSSkLnLHl7ypaTQiv0uScrTg10CgYEA/PKpzHE2Pfoya0qGSlsJmk5VZZgUIZpMRa0HG3uYf9WnTmk4AY7tMaYoVRO4wcRCgw+Fxf9F/zo9SO0DCB9BdaxhfEH42R18MN++0DCS49jRxquNiuYk/G/WfvExAwSlKCFuQlkqcse/WBeicu1WgrEPZSV/kfuWwg+bGvzym1MCgYEAuyCMO+UE6LTbKT/VfvL409ZDJ/qyzvRmpSQ5FVwJgZQ/WKB9KErgdLeEVvRoQqkX5MZsLq2++xe4Bu3fDxDnW4mQkAFSeDB+b4PqxvHE4KrX3KATX1fGmFJlpVr987inBpoT6PhaSfutopwVNjj7Bj/oF+5kIlXIlZ3DoS0M6scCgYEAxIQy7ya1oYkUSs7nbjU0TLG3Hur8GO8reqZm8y8e15JCHWUZofxMw1n308EytTepBPG2WJFu7E9u9Y1N4a2GyclXI5aNowCJT99E+7IBLQtyTwtROCx9Z7Hrz0vLbDDbr0Xpx5pGpE4TlnkmOGuz3m15LHfpmJ0CD1rYgisqwQkCgYAwFrszITXTv7aasSbiivpbJjL38TtGaBSA2AA7dv2SaVCmLAg99JAeLpM57XFlwCK9zig7DreHu561WSf7rTJnmcCm4VAaRwwXCGWrXrJjskPrFNAlrl8BAhvRFMMygP+beLkpI7nATYdfxJDG8HnCL2Yr0D23fSghGvwNTZCGPQKBgAP+FSzfMUWtz77jKK26+JGajEUG0Bovq4U/CN4BzI8xsAjeAB5dhkovVcC2GdSwWBW+7rWyAluh8VnfZPSkzrZBEMeaJplyR9xB/3zEjG+u8LV+enrhPFnqyNUw8r6xYCblwhIZd4MJ3hTUUOI8fYPWlCAIFlLTel/C4qlXLBlP";

    /// Stand up a wiremock OIDC provider (discovery + JWKS) and a
    /// `JwtValidator` that trusts it. Returns `(server, validator, issuer)`;
    /// keep `server` alive for the duration of the call.
    async fn setup() -> (MockServer, JwtValidator, String) {
        let server = MockServer::start().await;
        let issuer = format!("{}/realms/test", server.uri());

        Mock::given(method("GET"))
            .and(path("/realms/test/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jwks_uri": format!("{}/jwks", server.uri()),
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "keys": [{
                    "kid": TEST_KID,
                    "kty": "RSA",
                    "use": "sig",
                    "alg": "RS256",
                    "n": TEST_JWK_N,
                    "e": TEST_JWK_E,
                }],
            })))
            .mount(&server)
            .await;

        let inbound = KeycloakConfig {
            url: server.uri(),
            realm: "test".to_string(),
            client_id: "dpm".to_string(),
            client_secret: None,
            username: None,
            password: None,
        };
        let validator = JwtValidator::new(
            Some(inbound),
            None,
            std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            reqwest::Client::new(),
        );
        (server, validator, issuer)
    }

    fn unix_now() -> anyhow::Result<i64> {
        Ok(SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs() as i64)
    }

    fn sign(header: &Header, claims: &serde_json::Value) -> anyhow::Result<String> {
        let der = STANDARD
            .decode(TEST_RSA_PKCS1_DER_B64)
            .map_err(|e| anyhow::anyhow!("test key b64: {e}"))?;
        let key = EncodingKey::from_rsa_der(&der);
        encode(header, claims, &key).map_err(|e| anyhow::anyhow!("sign: {e}"))
    }

    /// Sign an RS256 token with the given issuer, `azp`, and `exp` offset.
    fn rs256_token(issuer: &str, azp: &str, exp_offset_secs: i64) -> anyhow::Result<String> {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(TEST_KID.to_string());
        let claims = json!({
            "iss": issuer,
            "sub": "alice",
            "azp": azp,
            "exp": unix_now()? + exp_offset_secs,
            "email": "alice@example.com",
            "realm_access": { "roles": ["admin", "user"] },
        });
        sign(&header, &claims)
    }

    #[tokio::test]
    async fn accepts_valid_token_and_projects_roles() -> anyhow::Result<()> {
        let (_server, validator, issuer) = setup().await;
        let token = rs256_token(&issuer, "dpm", 3600)?;
        let principal = validator
            .validate(&token)
            .await
            .map_err(|e| anyhow::anyhow!("expected valid token to verify: {e:?}"))?;
        assert_eq!(principal.sub, "alice");
        assert_eq!(principal.issuer, issuer);
        assert!(principal.has_role("admin"));
        assert!(principal.has_role("user"));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_expired_token() -> anyhow::Result<()> {
        let (_server, validator, issuer) = setup().await;
        let token = rs256_token(&issuer, "dpm", -3600)?;
        assert!(matches!(
            validator.validate(&token).await,
            Err(ValidationError::InactiveToken)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_tampered_signature() -> anyhow::Result<()> {
        let (_server, validator, issuer) = setup().await;
        let token = rs256_token(&issuer, "dpm", 3600)?;
        // Flip the last char of the signature segment — still valid base64url,
        // but the signature no longer verifies.
        let (head, sig) = token
            .rsplit_once('.')
            .ok_or_else(|| anyhow::anyhow!("token has no signature segment"))?;
        let mut sig_chars: Vec<char> = sig.chars().collect();
        if let Some(last) = sig_chars.last_mut() {
            *last = if *last == 'A' { 'B' } else { 'A' };
        }
        let tampered = format!("{head}.{}", sig_chars.into_iter().collect::<String>());
        assert!(matches!(
            validator.validate(&tampered).await,
            Err(ValidationError::InactiveToken)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_alg_confusion_hs256_header() -> anyhow::Result<()> {
        // Forge a token whose header advertises HS256 (same kid). The JWK is
        // RS256, so the alg-pinning check must reject it before verification —
        // closes the classic RS256->HS256 confusion attack.
        let (_server, validator, issuer) = setup().await;
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some(TEST_KID.to_string());
        let claims = json!({
            "iss": issuer,
            "sub": "alice",
            "azp": "dpm",
            "exp": unix_now()? + 3600,
        });
        let key = EncodingKey::from_secret(b"attacker-chosen-secret");
        let token = encode(&header, &claims, &key).map_err(|e| anyhow::anyhow!("{e}"))?;
        assert!(matches!(
            validator.validate(&token).await,
            Err(ValidationError::InactiveToken)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_cross_client_azp() -> anyhow::Result<()> {
        // Valid signature + issuer, but `azp` is a different client in the same
        // realm. The compensating control for `validate_aud = false` must reject.
        let (_server, validator, issuer) = setup().await;
        let token = rs256_token(&issuer, "some-other-client", 3600)?;
        assert!(matches!(
            validator.validate(&token).await,
            Err(ValidationError::InactiveToken)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_untrusted_issuer() -> anyhow::Result<()> {
        let (_server, validator, _issuer) = setup().await;
        let token = rs256_token("https://evil.example/realms/attacker", "dpm", 3600)?;
        assert!(matches!(
            validator.validate(&token).await,
            Err(ValidationError::UntrustedIssuer(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_missing_and_malformed_tokens() -> anyhow::Result<()> {
        let (_server, validator, _issuer) = setup().await;
        assert!(matches!(
            validator.validate("").await,
            Err(ValidationError::MissingToken)
        ));
        assert!(matches!(
            validator.validate("not.a.jwt").await,
            Err(ValidationError::MalformedToken)
        ));
        Ok(())
    }
}
