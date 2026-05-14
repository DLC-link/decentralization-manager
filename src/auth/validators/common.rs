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
    fn collect_roles_merges_realm_scope_and_flat_roles() {
        let realm = RealmAccess {
            roles: vec!["admin".into(), "user".into()],
        };
        let flat: Vec<String> = vec!["user".into(), "viewer".into()];
        let roles = collect_roles(Some(&realm), Some(&flat), Some("openid email viewer"));
        assert_eq!(roles, vec!["admin", "user", "viewer", "openid", "email"]);
    }
}
