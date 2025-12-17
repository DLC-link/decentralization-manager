use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

/// Configuration for onboarding workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OnboardingConfig {
    /// Party ID prefix for the decentralized party (e.g., "xyz-network")
    pub party_id_prefix: String,
    _p: PhantomData<()>,
}

impl OnboardingConfig {
    pub fn new(party_id_prefix: String) -> Self {
        Self {
            party_id_prefix,
            _p: PhantomData,
        }
    }
}
