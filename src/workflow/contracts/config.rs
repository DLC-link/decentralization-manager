use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::participant_id::CantonId;

/// Configuration for contracts workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContractsConfig {
    /// Decentralized party ID to deploy contracts for
    pub decentralized_party_id: CantonId,
    _p: PhantomData<()>,
}

impl ContractsConfig {
    pub fn new(decentralized_party_id: CantonId) -> Self {
        Self {
            decentralized_party_id,
            _p: PhantomData,
        }
    }
}
