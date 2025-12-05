use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::participant_id::CantonId;

/// Configuration for kick workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KickConfig {
    /// Decentralized party ID to remove participants from
    pub decentralized_party_id: CantonId,

    /// Participant IDs to kick
    pub participant_ids: Vec<CantonId>,

    _p: PhantomData<()>,
}

impl KickConfig {
    pub fn new(decentralized_party_id: CantonId, participant_ids: Vec<CantonId>) -> Self {
        Self {
            decentralized_party_id,
            participant_ids,
            _p: PhantomData,
        }
    }
}
