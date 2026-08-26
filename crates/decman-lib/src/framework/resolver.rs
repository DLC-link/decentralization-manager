use common::api::PackageConfig;

/// Resolves a catalog package key (e.g. "governance_core") to the
/// `#package-name` ref a template id carries. DecMan implements this with
/// its `PackageConfig`; an integrator implements it however they configure
/// packages — or passes a bare `&str` (as `&"#my-pkg"`) when one ref serves
/// everything.
pub trait PackageResolver {
    fn package_ref(&self, key: &str) -> Option<&str>;
}

impl PackageResolver for PackageConfig {
    fn package_ref(&self, key: &str) -> Option<&str> {
        match key {
            "governance_action" => self.governance_action.as_deref(),
            "governance_core" => self.governance_core.as_deref(),
            "governance_rewards" => self.governance_rewards.as_deref(),
            "governance_token_custody" => self.governance_token_custody.as_deref(),
            "governance_utility_credential" => self.governance_utility_credential.as_deref(),
            "governance_utility_onboarding" => self.governance_utility_onboarding.as_deref(),
            "utility_credential" => self.utility_credential.as_deref(),
            "utility_credential_app" => self.utility_credential_app.as_deref(),
            "utility_registry" => self.utility_registry.as_deref(),
            "vault" => self.vault.as_deref(),
            "vault_governance" => self.vault_governance.as_deref(),
            _ => None,
        }
    }
}

/// A bare package ref: every key resolves to this one ref. The right shape
/// for an integrator whose templates all live in one package.
///
/// Implemented on `&str` rather than `str`: Rust only unsizes a *sized*
/// value to a trait object, and `str` itself is unsized, so a plain `&str`
/// could never coerce to `&dyn PackageResolver`. `&str` is sized (a
/// reference is always sized), so callers pass one ref to their string ref —
/// `&"#my-pkg"` — exactly as they would `&my_package_config`.
impl PackageResolver for &str {
    fn package_ref(&self, _key: &str) -> Option<&str> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packages() -> PackageConfig {
        PackageConfig {
            governance_core: Some("#governance-core-v1".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn package_config_resolves_a_known_configured_key() {
        assert_eq!(
            packages().package_ref("governance_core"),
            Some("#governance-core-v1")
        );
    }

    #[test]
    fn package_config_returns_none_for_an_unknown_key() {
        assert_eq!(packages().package_ref("not_a_real_key"), None);
    }

    #[test]
    fn package_config_returns_none_for_a_none_field() {
        assert_eq!(packages().package_ref("vault"), None);
    }

    #[test]
    fn str_resolves_to_itself_for_any_key() {
        let pkg = "#fake-pkg";
        assert_eq!(pkg.package_ref("governance_core"), Some("#fake-pkg"));
        assert_eq!(pkg.package_ref("anything_at_all"), Some("#fake-pkg"));
    }
}
