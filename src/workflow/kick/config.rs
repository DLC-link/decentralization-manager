use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

/// Configuration for kick workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KickConfig {
    /// Decentralized party ID to remove participant from
    pub decentralized_party_id: CantonId,

    /// Participant ID to kick
    pub participant_id: CantonId,

    /// Namespace fingerprint (DNS owner key) to remove
    pub namespace_fingerprint: String,

    /// New threshold after the kick (configured by user)
    pub new_threshold: i32,

    /// The party's threshold before the kick. Display-only — carried through
    /// to the workflow run card so the operator sees "old → new". Defaults to
    /// 0 for configs persisted before this field existed.
    #[serde(default)]
    pub previous_threshold: i32,

    /// Workflow instance name for directory organization (e.g., "xyz-network-kick-20260108-143052")
    pub instance_name: String,

    _p: PhantomData<()>,
}

impl KickConfig {
    pub fn new(
        decentralized_party_id: CantonId,
        participant_id: CantonId,
        namespace_fingerprint: String,
        new_threshold: i32,
        previous_threshold: i32,
        instance_name: String,
    ) -> Self {
        Self {
            decentralized_party_id,
            participant_id,
            namespace_fingerprint,
            new_threshold,
            previous_threshold,
            instance_name,
            _p: PhantomData,
        }
    }
}
