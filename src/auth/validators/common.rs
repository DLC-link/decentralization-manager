//! Helpers shared between `JwtValidator` and `OidcIntrospectionValidator`.
//!
//! Both validators consume bearer tokens, route by issuer, and project a
//! provider-neutral `Principal` out of OIDC-shaped claims. The three
//! helpers below were duplicated verbatim across the two files; lifting
//! them here keeps them honest.
//!
//! Visibility is `pub(super)`: only sibling validator modules use these.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::Deserialize;

use crate::{
    auth::validator::ValidationError,
    config::{Auth0Config, KeycloakConfig},
};

/// Keycloak's nested role carrier. Both `JwtValidator::Claims` and
/// `OidcIntrospectionValidator::IntrospectionResponse` embed this same
/// shape, so it lives here.
#[derive(Deserialize)]
pub(super) struct RealmAccess {
    #[serde(default)]
    pub roles: Vec<String>,
}

/// Canonical OIDC issuer for a Keycloak-shaped config. Keycloak issues
/// `{url}/realms/{realm}`. Other providers use different conventions but
/// our config is Keycloak-shaped today; non-Keycloak support plugs in by
/// adding a variant and adjusting this function.
pub(super) fn oidc_issuer_of(cfg: &KeycloakConfig) -> String {
    format!("{}/realms/{}", cfg.url.trim_end_matches('/'), cfg.realm)
}

/// Base URL the *server* fetches OIDC metadata from (discovery, JWKS,
/// introspection) for a Keycloak-shaped config. Mirrors [`oidc_issuer_of`] but
/// swaps in `internal_url` (the backchannel address) when it is set and
/// non-empty, so a server that cannot reach the public/tailnet `url` — which
/// remains the token `iss` — can still fetch metadata via an in-cluster
/// address. When `internal_url` is unset this returns exactly the issuer
/// string, so single-URL configs behave identically to before. Issuer matching
/// continues to use [`oidc_issuer_of`].
pub(super) fn oidc_discovery_base_of(cfg: &KeycloakConfig) -> String {
    let base = cfg
        .internal_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&cfg.url);
    format!("{}/realms/{}", base.trim_end_matches('/'), cfg.realm)
}

/// Force `url`'s scheme + host + port to match `authority_source`, preserving
/// path and query. Returns `url` unchanged if either input fails to parse.
///
/// Lets the server fetch JWKS / introspection from the same host it reached
/// discovery on (the internal host) instead of trusting the host advertised in
/// the discovery document, which — depending on Keycloak's hostname config —
/// can be the public frontend host an in-cluster pod cannot reach. The token
/// `iss` is validated separately and is unaffected.
pub(super) fn rewrite_authority(url: &str, authority_source: &str) -> String {
    let (Ok(mut target), Ok(src)) = (
        reqwest::Url::parse(url),
        reqwest::Url::parse(authority_source),
    ) else {
        return url.to_string();
    };
    if target.set_scheme(src.scheme()).is_err()
        || target.set_host(src.host_str()).is_err()
        || target.set_port(src.port()).is_err()
    {
        return url.to_string();
    }
    target.into()
}

/// Canonical OIDC issuer for an Auth0 tenant. Auth0 issues
/// `https://{domain}/` in the JWT, but `extract_issuer` strips trailing
/// slashes — match against the slash-less form so the comparison lines up.
pub(super) fn auth0_issuer_of(cfg: &Auth0Config) -> String {
    format!("https://{}", cfg.domain.trim_end_matches('/'))
}

/// Extract the `iss` claim from a JWT without verifying its signature.
/// Used only to route to the correct trusted config; the signature /
/// introspection step is the authoritative check.
///
/// # Errors
///
/// Returns `ValidationError::MalformedToken` if the token is not a
/// well-formed JWT (missing header or payload segment, payload not valid
/// base64 / JSON, or `iss` claim missing or not a string).
pub(super) fn extract_issuer(token: &str) -> Result<String, ValidationError> {
    let mut parts = token.split('.');
    let _header = parts.next().ok_or(ValidationError::MalformedToken)?;
    let payload = parts.next().ok_or(ValidationError::MalformedToken)?;

    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| {
            // Some providers use standard (padded) base64.
            let padding = (4 - payload.len() % 4) % 4;
            let padded = format!("{payload}{}", "=".repeat(padding));
            STANDARD.decode(padded)
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

/// Merge realm-access roles, flat `roles`, and space-separated `scope`
/// into a single deduped role list, preserving first-seen order.
///
/// Generalised over the two carrier shapes (`Claims` for JWT, `IntrospectionResponse`
/// for introspection) by taking the underlying fields directly.
pub(super) fn collect_roles(
    realm_access: Option<&RealmAccess>,
    flat_roles: Option<&[String]>,
    scope: Option<&str>,
) -> Vec<String> {
    let mut roles = Vec::new();
    if let Some(r) = realm_access {
        roles.extend(r.roles.iter().cloned());
    }
    if let Some(extra) = flat_roles {
        for role in extra {
            if !roles.contains(role) {
                roles.push(role.clone());
            }
        }
    }
    if let Some(scope) = scope {
        for s in scope.split_whitespace() {
            let s = s.to_string();
            if !roles.contains(&s) {
                roles.push(s);
            }
        }
    }
    roles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issuer_extraction_handles_url_safe_base64() {
        // {"iss":"https://keycloak.example.com/realms/foo","sub":"alice"}
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"iss":"https://keycloak.example.com/realms/foo","sub":"alice"}"#);
        let token = format!("header.{payload}.sig");
        assert_eq!(
            extract_issuer(&token).unwrap(),
            "https://keycloak.example.com/realms/foo"
        );
    }

    #[test]
    fn issuer_extraction_strips_trailing_slash() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"iss":"https://example.com/"}"#);
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
            internal_url: None,
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

    #[test]
    fn discovery_base_falls_back_to_url_when_internal_unset() {
        let cfg = KeycloakConfig {
            url: "https://kc.example.com".to_string(),
            internal_url: None,
            realm: "bitsafe".to_string(),
            client_id: "dpm".to_string(),
            client_secret: None,
            username: None,
            password: None,
        };
        // Unset internal_url → identical to the issuer, preserving old behavior.
        assert_eq!(oidc_discovery_base_of(&cfg), oidc_issuer_of(&cfg));
        assert_eq!(
            oidc_discovery_base_of(&cfg),
            "https://kc.example.com/realms/bitsafe"
        );
    }

    #[test]
    fn discovery_base_uses_internal_url_when_set() {
        let cfg = KeycloakConfig {
            url: "https://kc.public.example/".to_string(),
            internal_url: Some("http://kc.svc.cluster.local/".to_string()),
            realm: "bitsafe".to_string(),
            client_id: "dpm".to_string(),
            client_secret: None,
            username: None,
            password: None,
        };
        // Discovery targets the internal host; issuer stays the public URL.
        assert_eq!(
            oidc_discovery_base_of(&cfg),
            "http://kc.svc.cluster.local/realms/bitsafe"
        );
        assert_eq!(
            oidc_issuer_of(&cfg),
            "https://kc.public.example/realms/bitsafe"
        );
    }

    #[test]
    fn discovery_base_treats_empty_internal_url_as_unset() {
        let cfg = KeycloakConfig {
            url: "https://kc.example.com".to_string(),
            internal_url: Some(String::new()),
            realm: "bitsafe".to_string(),
            client_id: "dpm".to_string(),
            client_secret: None,
            username: None,
            password: None,
        };
        assert_eq!(oidc_discovery_base_of(&cfg), oidc_issuer_of(&cfg));
    }

    #[test]
    fn rewrite_authority_swaps_host_keeps_path() {
        // Discovery advertised a public host; we rewrite it to the internal one
        // while preserving the realm/certs path and query.
        assert_eq!(
            rewrite_authority(
                "https://public.example/realms/x/protocol/openid-connect/certs?foo=bar",
                "http://kc.svc.cluster.local:8080",
            ),
            "http://kc.svc.cluster.local:8080/realms/x/protocol/openid-connect/certs?foo=bar"
        );
    }

    #[test]
    fn rewrite_authority_is_noop_when_authority_matches() {
        assert_eq!(
            rewrite_authority("https://kc.example/realms/x/certs", "https://kc.example"),
            "https://kc.example/realms/x/certs"
        );
    }

    #[test]
    fn rewrite_authority_returns_input_on_unparseable() {
        assert_eq!(rewrite_authority("not a url", "http://kc.svc"), "not a url");
        assert_eq!(
            rewrite_authority("https://kc.example/certs", "not a url"),
            "https://kc.example/certs"
        );
    }

    #[test]
    fn collect_roles_merges_realm_scope_and_flat_roles() {
        let realm = RealmAccess {
            roles: vec!["admin".into(), "user".into()],
        };
        let flat: Vec<String> = vec!["user".into(), "viewer".into()];
        let roles = collect_roles(Some(&realm), Some(&flat), Some("openid email viewer"));
        assert_eq!(roles, vec!["admin", "user", "viewer", "openid", "email"]);
    }
}
