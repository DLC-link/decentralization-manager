//! Provider-agnostic inbound token validation.
//!
//! Separate from the outbound `TokenManager`/`WorkflowAuth` in this module:
//! those produce tokens we send to Canton. `TokenValidator` consumes tokens
//! clients send to us and yields a `Principal` the authorization layer works
//! against. Keeping `Principal` provider-neutral means swapping the IdP
//! (Keycloak, Auth0, Google, ...) does not ripple into handler code.

use std::sync::Arc;

use thiserror::Error;

#[cfg(any(test, feature = "test-mode"))]
use super::validators::MockValidator;
use super::validators::{JwtValidator, OidcIntrospectionValidator};

#[derive(Error, Debug)]
pub enum ValidationError {
    #[error("missing bearer token")]
    MissingToken,

    #[error("malformed bearer token")]
    MalformedToken,

    #[error("token issuer not trusted: {0}")]
    UntrustedIssuer(String),

    #[error("token is not active")]
    InactiveToken,

    #[error("OIDC discovery failed for {issuer}: {message}")]
    DiscoveryFailed { issuer: String, message: String },

    #[error("introspection request failed: {0}")]
    IntrospectionFailed(String),

    #[error("missing required role: {0}")]
    MissingRole(String),
}

/// Authenticated caller principal. Fields are the common OIDC subset that every
/// provider we care about supplies. Handlers should reason about `roles` and
/// `sub`, never provider-specific claim paths.
#[derive(Clone, Debug)]
pub struct Principal {
    /// Stable user id (`sub` claim).
    pub sub: String,
    /// Issuer the token came from (for audit + diagnostics).
    pub issuer: String,
    /// Roles collected from the token/introspection response.
    pub roles: Vec<String>,
    /// Email, if the provider exposes it.
    pub email: Option<String>,
}

impl Principal {
    /// Whether this principal carries the given role.
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    /// Gate an action on an admin role. The role name is configurable so
    /// deployments can map to whatever their IdP already issues.
    ///
    /// # Errors
    ///
    /// Returns `ValidationError::MissingRole` if the principal does not
    /// carry `admin_role`.
    pub fn require_admin(&self, admin_role: &str) -> Result<(), ValidationError> {
        if self.has_role(admin_role) {
            Ok(())
        } else {
            Err(ValidationError::MissingRole(admin_role.to_string()))
        }
    }

    /// Placeholder for per-party authorization. v1 is node-scoped: every
    /// authenticated caller can operate any party on this node. When that
    /// assumption breaks (first shared-node deployment), fill this body in
    /// against a `party_members` table — no handler changes required.
    ///
    /// # Errors
    ///
    /// Currently infallible. Declared fallible so call sites stay correct
    /// when the body is filled in later.
    pub fn require_party_access(
        &self,
        _party_id: &crate::canton_id::CantonId,
    ) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Inbound token validator. Enum rather than trait object to mirror the
/// existing `WorkflowAuth` pattern and avoid pulling in `async-trait`.
#[derive(Clone)]
pub enum TokenValidator {
    /// Local JWT signature verification against the IdP's JWKS. No
    /// server-to-server call on the hot path.
    Jwt(Arc<JwtValidator>),
    /// RFC 7662 introspection against a real OIDC provider. Kept for
    /// deployments where local signature verification is not feasible.
    OidcIntrospection(Arc<OidcIntrospectionValidator>),
    /// Permissive dev/test validator (admin-by-default, accepts any
    /// token). Compiled in only behind `cfg(any(test, feature = "test-mode"))`
    /// so a production binary cannot accidentally enable it.
    #[cfg(any(test, feature = "test-mode"))]
    Mock(Arc<MockValidator>),
}

impl TokenValidator {
    /// Validate a bearer token and produce the caller's `Principal`.
    ///
    /// # Errors
    ///
    /// Returns a `ValidationError` variant describing why the token was
    /// rejected (missing, malformed, untrusted issuer, inactive, introspection
    /// call failed).
    pub async fn validate(&self, token: &str) -> Result<Principal, ValidationError> {
        match self {
            Self::Jwt(v) => v.validate(token).await,
            Self::OidcIntrospection(v) => v.validate(token).await,
            #[cfg(any(test, feature = "test-mode"))]
            Self::Mock(v) => v.validate(token).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal_with_roles(roles: &[&str]) -> Principal {
        Principal {
            sub: "alice".to_string(),
            issuer: "https://idp.example/realms/test".to_string(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            email: None,
        }
    }

    #[test]
    fn has_role_is_exact_match() {
        let p = principal_with_roles(&["admin", "viewer"]);
        assert!(p.has_role("admin"));
        assert!(p.has_role("viewer"));
        // Not a prefix, suffix, substring, or case-insensitive match — an
        // attacker holding a "adm" or "Admin" role must not pass an "admin" gate.
        assert!(!p.has_role("adm"));
        assert!(!p.has_role("admin2"));
        assert!(!p.has_role("Admin"));
        assert!(!p.has_role(""));
    }

    #[test]
    fn require_admin_rejects_principal_without_role() {
        // The privilege-escalation guard: an authenticated caller that does
        // not carry the admin role must be refused.
        let p = principal_with_roles(&["viewer", "operator"]);
        assert!(matches!(
            p.require_admin("admin"),
            Err(ValidationError::MissingRole(role)) if role == "admin"
        ));
    }

    #[test]
    fn require_admin_rejects_principal_with_no_roles() {
        let p = principal_with_roles(&[]);
        assert!(p.require_admin("admin").is_err());
    }

    #[test]
    fn require_admin_accepts_principal_with_role() {
        let p = principal_with_roles(&["admin"]);
        assert!(p.require_admin("admin").is_ok());
    }
}
