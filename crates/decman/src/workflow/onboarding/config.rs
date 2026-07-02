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
    _p: PhantomData<()>,
}

impl OnboardingConfig {
    pub fn new(party_id_prefix: String, instance_name: String, threshold: Option<i32>) -> Self {
        Self {
            party_id_prefix,
            instance_name,
            threshold,
            _p: PhantomData,
        }
    }

    /// Get the namespace key name derived from party_id_prefix
    pub fn namespace_key_name(&self) -> String {
        format!("{}-namespace", self.party_id_prefix)
    }

    /// Get the DAML key name derived from party_id_prefix
    pub fn daml_key_name(&self) -> String {
        format!("{}-daml-transactions", self.party_id_prefix)
    }
}
