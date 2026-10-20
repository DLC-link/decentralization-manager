use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

/// Configuration for onboarding workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OnboardingConfig {
    /// Party ID prefix for the decentralized party (e.g., "xyz-network")
    pub party_id_prefix: String,
    /// Workflow instance name for directory organization (e.g., "xyz-network-creation")
    pub instance_name: String,
    /// Operator-chosen initial threshold. `None` means "use the default
    /// majority algorithm" (`ceil(owner_count / 2)`, min 1), computed by the
    /// coordinator once the owner set is known. Defaults to `None` for configs
    /// persisted before this field existed.
    #[serde(default)]
    pub threshold: Option<i32>,
    /// Operator-chosen confirmation threshold. `None` means "the same as the
    /// namespace threshold", which is also what configs persisted before this
    /// field existed decode to.
    #[serde(default)]
    pub confirmation_threshold: Option<i32>,
    _p: PhantomData<()>,
}

impl OnboardingConfig {
    pub fn new(
        party_id_prefix: String,
        instance_name: String,
        threshold: Option<i32>,
        confirmation_threshold: Option<i32>,
    ) -> Self {
        Self {
            party_id_prefix,
            instance_name,
            threshold,
            confirmation_threshold,
            _p: PhantomData,
        }
    }

    /// Get the namespace key name derived from party_id_prefix
    pub fn namespace_key_name(&self) -> String {
        format!("{}-namespace", self.party_id_prefix)
    }

    /// Get the Daml key name derived from party_id_prefix
    pub fn daml_key_name(&self) -> String {
        format!("{}-daml-transactions", self.party_id_prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run persisted before the confirmation threshold existed resumes
    /// with both thresholds equal, as it was started.
    #[test]
    fn a_config_persisted_before_the_confirmation_threshold_still_decodes() -> crate::error::Result
    {
        let config: OnboardingConfig = serde_json::from_str(
            r#"{"party_id_prefix":"xyz","instance_name":"xyz-creation","threshold":2,"_p":null}"#,
        )?;
        assert_eq!(config.threshold, Some(2));
        assert_eq!(config.confirmation_threshold, None);
        Ok(())
    }
}
