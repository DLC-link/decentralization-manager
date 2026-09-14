use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::{
    canton_id::CantonId,
    workflow::{
        party_replication::{ArtifactStore, ReplicationArtifacts, ReplicationTarget},
        storage::artifact_kinds,
    },
};

/// The artifact keys the add-party run's replication has always used. Named
/// here rather than inside the replication core so the core stays free of any
/// one workflow's key namespace — and so these strings keep their exact values,
/// which is what lets a run interrupted before the extraction resume after it.
pub const ADD_PARTY_REPLICATION_ARTIFACTS: ReplicationArtifacts = ReplicationArtifacts {
    export_offset: artifact_kinds::ADD_PARTY_EXPORT_OFFSET,
    pre_activation_offset: artifact_kinds::ADD_PARTY_PRE_ACTIVATION_OFFSET,
    import_inflight: artifact_kinds::ADD_PARTY_ACS_IMPORT_INFLIGHT,
};

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

    /// Daml signing key name for the new member (see `namespace_key_name`).
    pub fn daml_key_name(&self) -> String {
        format!("{}-daml-transactions", self.decentralized_party_id.prefix)
    }

    /// This run's replication, addressed the way the party-type-agnostic core
    /// expects: the decentralized party moving onto the new member.
    ///
    /// `instance_name` is passed rather than read off `self` because the step
    /// callers already carry it, and the two must not be allowed to disagree.
    pub fn replication_target(&self, instance_name: &str) -> ReplicationTarget {
        ReplicationTarget::new(
            self.decentralized_party_id.clone(),
            self.new_participant_id.clone(),
            instance_name.to_string(),
            ADD_PARTY_REPLICATION_ARTIFACTS,
            ArtifactStore::WorkflowRun,
        )
    }
}
