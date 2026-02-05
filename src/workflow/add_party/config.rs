use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::participant_id::CantonId;

/// Configuration for add party workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AddPartyConfig {
    /// Decentralized party ID to add participant to
    pub decentralized_party_id: CantonId,

    /// Participant ID of the new member being added
    pub new_participant_id: CantonId,

    /// New threshold after adding the member (configured by user)
    pub new_threshold: i32,

    /// Workflow instance name for directory organization (e.g., "xyz-network-add-party-20260108-143052")
    pub instance_name: String,

    _p: PhantomData<()>,
}

impl AddPartyConfig {
    pub fn new(
        decentralized_party_id: CantonId,
        new_participant_id: CantonId,
        new_threshold: i32,
        instance_name: String,
    ) -> Self {
        Self {
            decentralized_party_id,
            new_participant_id,
            new_threshold,
            instance_name,
            _p: PhantomData,
        }
    }

    /// Generate a unique key name for the namespace signing key
    pub fn namespace_key_name(&self) -> String {
        format!(
            "{party_id_prefix}-namespace-key",
            party_id_prefix = self.decentralized_party_id.prefix
        )
    }

    /// Generate a unique key name for the DAML signing key
    pub fn daml_key_name(&self) -> String {
        format!(
            "{party_id_prefix}-daml-key",
            party_id_prefix = self.decentralized_party_id.prefix
        )
    }
}
