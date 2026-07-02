use crate::auth::{
    mock::{MOCK_TOKEN, MOCK_USER_ID},
    validator::{Principal, ValidationError},
};

/// Permissive inbound validator, selected at runtime in insecure mode
/// (`--insecure` / `DECPM_INSECURE`, or under `cargo test`).
///
/// Accepts *any* token, including an empty/missing one — validation always
/// succeeds and only logs when the token differs from the mock token. So the
/// insecure-mode UX (swagger, unauthenticated curls) works while the
/// middleware plumbing is still exercised end-to-end. The returned principal
/// carries the admin role so gated endpoints (`PUT /party-config`, `POST /kick`)
/// are reachable without a real IdP.
pub struct MockValidator {
    admin_role: String,
}

impl MockValidator {
    /// Create a new mock validator that mints principals carrying `admin_role`.
    pub fn new(admin_role: String) -> Self {
        Self { admin_role }
    }

    /// Accept any token and return a principal carrying the admin role.
    ///
    /// # Errors
    ///
    /// Never returns an error in the current implementation — kept fallible
    /// so the signature matches the other validators behind `TokenValidator`.
    pub async fn validate(&self, token: &str) -> Result<Principal, ValidationError> {
        // Accept anything in test mode — dev ergonomics take priority over
        // strictness. Production runs never select this validator.
        if !token.is_empty() && token != MOCK_TOKEN {
            tracing::debug!("MockValidator: accepting non-mock token in test mode");
        }
        Ok(Principal {
            sub: MOCK_USER_ID.to_string(),
            issuer: "mock".to_string(),
            roles: vec![self.admin_role.clone()],
            email: None,
        })
    }
}
