use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::participant_id::CantonId;

/// Configuration for kick workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KickConfig {
    /// Decentralized party ID to remove participant from
    pub decentralized_party_id: CantonId,

    /// Participant ID to kick
    pub participant_id: CantonId,

    /// Namespace fingerprint (DNS owner key) to remove
    pub namespace_fingerprint: String,

    _p: PhantomData<()>,
}

impl KickConfig {
    pub fn new(
        decentralized_party_id: CantonId,
        participant_id: CantonId,
        namespace_fingerprint: String,
    ) -> Self {
        Self {
            decentralized_party_id,
            participant_id,
            namespace_fingerprint,
            _p: PhantomData,
        }
    }
}
