use serde::{Deserialize, Serialize};

use crate::{
    canton_id::{CantonId, Fingerprint},
    workflow::{signing_keys::rotated_daml_key_name, thresholds::PartyThresholds},
};

/// Configuration for the rotate-key workflow.
///
/// The coordinator replaces its own Daml signing key for the party with a new
/// key from its vault. Members, owners and thresholds stay the same. The
/// thresholds are read from the chain when the run starts and pinned here, so
/// a peer can hold the proposal to what its operator accepted.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RotateKeyConfig {
    /// Decentralized party whose key set changes.
    pub decentralized_party_id: CantonId,
    /// The coordinator's current Daml key, which leaves the party.
    pub rotated_key: Fingerprint,
    /// The namespace threshold, which the party's signing keys also use.
    pub threshold: i32,
    /// The confirmation threshold of the party's P2P mapping.
    pub confirmation_threshold: i32,
    /// Workflow instance name (e.g. "xyz-rotate-key-1736345452").
    pub instance_name: String,
}

impl RotateKeyConfig {
    /// The vault name of the new key. It depends on the key being replaced,
    /// so every run that replaces that key reuses the same new key.
    pub fn new_key_name(&self) -> String {
        rotated_daml_key_name(&self.decentralized_party_id.prefix, &self.rotated_key)
    }

    /// The thresholds the proposal keeps.
    pub fn thresholds(&self) -> PartyThresholds {
        PartyThresholds::new(self.threshold, Some(self.confirmation_threshold))
    }
}
