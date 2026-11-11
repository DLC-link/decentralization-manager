use std::collections::BTreeSet;

use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId, config::NodeConfig, db::schema::SchemaRead, error::Result,
    server::WorkflowKind,
};

/// What the operator agreed to when they accepted the invitation.
///
/// Built from the peer's own `workflow_runs` row, which
/// [`crate::server::handlers::invitations::insert_peer_run`] writes at accept
/// time from the invitation payload. That row — not anything the coordinator
/// sends later — is the reference every check compares against.
#[derive(Clone, Debug)]
pub struct PeerExpectations {
    /// The workflow kind the operator accepted. Commands belonging to any
    /// other kind are refused outright.
    pub kind: WorkflowKind,
    pub contract_intents: Vec<common::api::ContractDeploymentIntent>,
    /// The full member set: the participants named in the invitation plus the
    /// coordinator itself (onboarding and kick invitations list only the
    /// invitees; add-party and change-threshold already include the
    /// coordinator, so the union is the same set either way).
    pub members: BTreeSet<CantonId>,
    /// The dec party the run targets. Absent for onboarding, where the party
    /// does not exist yet.
    pub dec_party_id: Option<CantonId>,
    /// Onboarding only: the party id prefix the coordinator proposed.
    pub prefix: Option<String>,
    /// The threshold the topology should end up with.
    pub threshold: Option<i32>,
    /// The confirmation threshold the P2P mapping should end up with. `None`
    /// means it equals `threshold`, which is all an older coordinator sends.
    pub confirmation_threshold: Option<i32>,
    /// Kick only: the participant being removed.
    pub kicked_participant: Option<CantonId>,
    /// Kick only: the namespace owner key being removed. Absent from
    /// coordinators that predate the field.
    pub kicked_owner_key: Option<String>,
    /// Add-party only: the participant being added.
    pub new_participant: Option<CantonId>,
    /// Dars only: the filenames the operator agreed to install.
    pub dar_filenames: Vec<String>,
    /// Dars only: SHA-256 of each accepted DAR, index-aligned with
    /// `dar_filenames`. Empty when the coordinator predates the field.
    pub dar_hashes: Vec<String>,
    /// This node's own participant id.
    pub self_id: CantonId,
}

/// The invitation fields `insert_peer_run` stores on the run row.
#[derive(serde::Deserialize)]
struct PeerRunConfig {
    #[serde(default)]
    contract_intents: Vec<common::api::ContractDeploymentIntent>,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    participants: Vec<CantonId>,
    #[serde(default)]
    dar_filenames: Vec<String>,
    #[serde(default)]
    dar_hashes: Vec<String>,
    /// The kicked participant, stored under the coordinator's field name.
    #[serde(default)]
    participant_id: Option<CantonId>,
    /// The kicked owner key, stored under the coordinator's field name.
    #[serde(default)]
    namespace_fingerprint: Option<String>,
    #[serde(default)]
    new_participant_id: Option<CantonId>,
    #[serde(default)]
    new_threshold: Option<i32>,
    #[serde(default)]
    confirmation_threshold: Option<i32>,
}

impl PeerExpectations {
    /// Load the accepted invitation for a peer run.
    ///
    /// # Errors
    ///
    /// Errors if the run row is missing or its stored invitation cannot be
    /// decoded. Both mean the peer cannot tell what its operator agreed to,
    /// and a peer that cannot check a payload must not sign it.
    pub async fn load(
        db: &SqlitePool,
        instance_name: &str,
        node_config: &NodeConfig,
        coordinator_id: &CantonId,
    ) -> Result<Self> {
        let run = db.get_workflow_run(instance_name).await?.ok_or_else(|| {
            anyhow::anyhow!("no workflow run {instance_name} to validate against")
        })?;

        let config: PeerRunConfig = serde_json::from_str(&run.config_json).map_err(|e| {
            anyhow::anyhow!("cannot decode the accepted invitation for {instance_name}: {e}")
        })?;

        let mut members: BTreeSet<CantonId> = config.participants.into_iter().collect();
        members.insert(coordinator_id.clone());

        Ok(Self {
            kind: run.kind,
            contract_intents: config.contract_intents,
            members,
            dec_party_id: run.dec_party_id,
            prefix: config.prefix,
            threshold: config.new_threshold,
            confirmation_threshold: config.confirmation_threshold,
            kicked_participant: config.participant_id,
            kicked_owner_key: config.namespace_fingerprint,
            new_participant: config.new_participant_id,
            dar_filenames: config.dar_filenames,
            dar_hashes: config.dar_hashes,
            self_id: node_config.participant_id().clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Peer runs accepted before the field existed have no confirmation
    /// threshold in their stored invitation, and still load.
    #[test]
    fn a_stored_invitation_without_a_confirmation_threshold_still_decodes() -> Result {
        let before: PeerRunConfig = serde_json::from_str(
            r#"{"prefix":null,"participants":[],"dar_filenames":[],"dar_hashes":[],
               "participant_id":null,"new_participant_id":null,"new_threshold":2,
               "previous_threshold":3,"package_names":[],"contract_intents":[]}"#,
        )?;
        assert_eq!(before.new_threshold, Some(2));
        assert_eq!(before.confirmation_threshold, None);

        let after: PeerRunConfig =
            serde_json::from_str(r#"{"new_threshold":2,"confirmation_threshold":3}"#)?;
        assert_eq!(after.confirmation_threshold, Some(3));
        Ok(())
    }
}
