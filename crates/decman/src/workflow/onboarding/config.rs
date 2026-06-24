use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

/// Configuration for onboarding workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OnboardingConfig {
    /// Party ID prefix for the decentralized party (e.g., "xyz-network")
    pub party_id_prefix: String,
    /// Workflow instance name for directory organization (e.g., "xyz-network-creation")
    pub instance_name: String,
    _p: PhantomData<()>,
}

impl OnboardingConfig {
    pub fn new(party_id_prefix: String, instance_name: String) -> Self {
        Self {
            party_id_prefix,
            instance_name,
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
