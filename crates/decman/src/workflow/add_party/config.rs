use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

/// Configuration for the add-party workflow (adding a new member to an
/// existing decentralized party)
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AddPartyConfig {
    /// Decentralized party ID the new member is being added to
    pub decentralized_party_id: CantonId,

    /// Participant ID of the new member
    pub new_participant_id: CantonId,

    /// New threshold after the add (configured by user)
    pub new_threshold: i32,

    /// The party's threshold before the add. Display-only — carried through
    /// to the workflow run card so the operator sees "old → new". Defaults to
    /// 0 for configs persisted before this field existed.
    #[serde(default)]
    pub previous_threshold: i32,

    /// Workflow instance name (e.g., "xyz-network-add-party-1717000000")
    pub instance_name: String,

    _p: PhantomData<()>,
}

impl AddPartyConfig {
    pub fn new(
        decentralized_party_id: CantonId,
        new_participant_id: CantonId,
        new_threshold: i32,
        previous_threshold: i32,
        instance_name: String,
    ) -> Self {
        Self {
            decentralized_party_id,
            new_participant_id,
            new_threshold,
            previous_threshold,
            instance_name,
            _p: PhantomData,
        }
    }

    /// Namespace key name for the new member — same derivation onboarding
    /// uses, so a member added later is indistinguishable from a founding one.
    pub fn namespace_key_name(&self) -> String {
        format!("{}-namespace", self.decentralized_party_id.prefix)
    }

    /// DAML signing key name for the new member (see `namespace_key_name`).
    pub fn daml_key_name(&self) -> String {
        format!("{}-daml-transactions", self.decentralized_party_id.prefix)
    }
}
