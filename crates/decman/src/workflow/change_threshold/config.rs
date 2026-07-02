use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

/// Configuration for the change-threshold workflow.
///
/// Changes the signing threshold of an existing decentralized party without
/// altering its membership: the `DecentralizedNamespaceDefinition.threshold`
/// (how many namespace owners must sign topology transactions) and the
/// matching `PartyToParticipant.threshold` (party confirmation threshold) are
/// re-issued with `new_threshold`, keeping the same owners/participants.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChangeThresholdConfig {
    /// Decentralized party whose threshold is being changed.
    pub decentralized_party_id: CantonId,

    /// New threshold to set on both the DNS and P2P mappings.
    pub new_threshold: i32,

    /// The party's threshold before the change. Display-only — carried through
    /// to the workflow run card so the operator sees "old → new". Defaults to
    /// 0 for configs persisted before this field existed.
    #[serde(default)]
    pub previous_threshold: i32,

    /// Workflow instance name for artefact organisation
    /// (e.g. "xyz-change-threshold-20260108-143052").
    pub instance_name: String,

    _p: PhantomData<()>,
}

impl ChangeThresholdConfig {
    pub fn new(
        decentralized_party_id: CantonId,
        new_threshold: i32,
        previous_threshold: i32,
        instance_name: String,
    ) -> Self {
        Self {
            decentralized_party_id,
            new_threshold,
            previous_threshold,
            instance_name,
            _p: PhantomData,
        }
    }
}
