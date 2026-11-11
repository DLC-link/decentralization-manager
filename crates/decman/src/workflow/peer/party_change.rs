//! The membership-change commands: sign a kick, change-threshold or
//! add-party proposal pair, and retire legacy keys.

use sqlx::SqlitePool;

use super::{
    payload::{decode_add_party_config, is_new_member},
    run::{PeerRun, StepError, StepResult},
};
use crate::{
    config::NodeConfig,
    error::Result,
    noise::{MessageType, client::NoiseClient},
    utils,
    workflow::{add_party, change_threshold, kick, onboarding, party_replication, topology},
};

impl PeerRun {
    /// RetireLegacyKeys: retire the legacy keys of the accepted party.
    pub(super) async fn retire_legacy_keys(&mut self) -> StepResult {
        // The target comes from the accepted invitation, never this command's payload.
        let party = self
            .expectations
            .dec_party_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No accepted party for legacy key retirement"))?;
        let synchronizer_id = utils::get_synchronizer_id(&self.node_config).await?;
        topology::retire_legacy_keys(&self.node_config, &synchronizer_id, party).await?;
        self.consecutive_step_failures = 0;
        self.client
            .send(
                MessageType::StatusUpdate,
                b"RetireLegacyKeys completed".to_vec(),
            )
            .await?;
        Ok(())
    }

    /// Sign the DNS + P2P proposal pair of a membership change: the
    /// SignKick, SignChangeThreshold and SignAddParty commands. Only an
    /// add-party run names a new member, and only the new member does more
    /// than sign.
    pub(super) async fn sign_membership(
        &mut self,
        membership: Membership,
        payload: &[u8],
    ) -> StepResult {
        let label = membership.label();
        tracing::info!("Executing: Sign {label} proposals");
        if payload.is_empty() {
            return Err(StepError::Retry(format!(
                "No {label} proposals payload received from coordinator"
            )));
        }

        let items = match utils::decode_length_prefixed(payload, 3) {
            Ok(items) => items,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Failed to decode {command} payload: {e}",
                    command = membership.command()
                )));
            }
        };
        let add_party_config = membership.decode_config(&items[0])?;

        let proposals_check = async {
            if let Some(add_party_config) = &add_party_config {
                self.expectations
                    .check_dec_party(&add_party_config.decentralized_party_id)?;
                self.expectations
                    .check_new_participant(&add_party_config.new_participant_id)?;
            }
            self.expectations
                .check_party_proposals(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &items[1],
                    &items[2],
                )
                .await
        }
        .await;
        if let Err(e) = proposals_check {
            return Err(StepError::Retry(format!(
                "Refusing the coordinator's {label} proposals: {e}"
            )));
        }

        let new_member = add_party_config.filter(|config| is_new_member(&self.node_config, config));
        if new_member.is_some()
            && let Err(e) = party_replication::check_vetted_packages_held(&self.node_config).await
        {
            return Err(StepError::Retry(format!(
                "Refusing to sign as the new member: {e:#}"
            )));
        }

        let proposal_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
        if let Err(e) = membership
            .sign_proposals(
                &self.node_config,
                &self.db,
                &self.instance_name,
                &proposal_data,
            )
            .await
        {
            return Err(StepError::Retry(format!("Step execution failed: {e}")));
        }
        // The new member leaves the synchronizer before its signature can
        // let the coordinator authorize the mapping. From the moment the
        // mapping is effective it receives the party's traffic, and it
        // must not journal any of it until the ACS import has landed.
        // Its own store has to hold the proposal first: disconnected, it
        // will not see the mapping take effect, and the import checks for it.
        if let Some(add_party_config) = &new_member
            && let Err(e) = async {
                add_party::peer::wait_for_hosting_proposal(
                    &self.node_config,
                    &add_party_config.decentralized_party_id,
                )
                .await?;
                party_replication::open_import_window(
                    &self.node_config,
                    &self.db,
                    &add_party_config.replication_target(&self.instance_name),
                )
                .await
            }
            .await
        {
            return Err(StepError::Retry(format!(
                "Failed to open the ACS import window: {e:#}"
            )));
        }
        self.consecutive_step_failures = 0;
        if let Err(e) = membership
            .send_signatures(
                &self.client,
                &self.db,
                &self.instance_name,
                &self.node_config,
            )
            .await
        {
            tracing::error!("Failed to send {label} signatures to coordinator: {e}");
        }

        // Identity hook (new member): same point in the protocol
        // where onboarding peers persist their identity — the party
        // id is authoritative from the config, and the keys were
        // written by GenerateAddPartyKeys.
        if let Some(add_party_config) = &new_member
            && let Err(e) = onboarding::peer::copy_self_identity_for_party(
                &self.db,
                &self.instance_name,
                &self.node_config,
                &add_party_config.decentralized_party_id,
            )
            .await
        {
            tracing::error!(
                "Failed to copy self identity for {party}: {e}",
                party = add_party_config.decentralized_party_id
            );
        }
        Ok(())
    }
}

/// The membership changes whose peers sign a DNS + P2P proposal pair.
#[derive(Clone, Copy)]
pub(super) enum Membership {
    Kick,
    ChangeThreshold,
    AddParty,
}

impl Membership {
    /// The workflow, as the sign step's log lines and errors name it.
    fn label(self) -> &'static str {
        match self {
            Self::Kick => "kick",
            Self::ChangeThreshold => "change-threshold",
            Self::AddParty => "add-party",
        }
    }

    /// The sign command, as its decode error names it.
    fn command(self) -> &'static str {
        match self {
            Self::Kick => "SignKick",
            Self::ChangeThreshold => "SignChangeThreshold",
            Self::AddParty => "SignAddParty",
        }
    }

    /// Decode the config the sign command carries. Kick and change-threshold
    /// only check that it parses. Add-party returns it, because the step
    /// checks its party and new member too.
    fn decode_config(self, config: &[u8]) -> StepResult<Option<add_party::AddPartyConfig>> {
        match self {
            Self::Kick => {
                let _kick_config: kick::KickConfig = match serde_json::from_slice(config) {
                    Ok(config) => config,
                    Err(e) => {
                        return Err(StepError::Retry(format!(
                            "Failed to deserialize kick config: {e}"
                        )));
                    }
                };
                Ok(None)
            }
            Self::ChangeThreshold => {
                let _change_config: change_threshold::ChangeThresholdConfig =
                    match serde_json::from_slice(config) {
                        Ok(config) => config,
                        Err(e) => {
                            return Err(StepError::Retry(format!(
                                "Failed to deserialize change-threshold config: {e}"
                            )));
                        }
                    };
                Ok(None)
            }
            Self::AddParty => match decode_add_party_config(config) {
                Ok(config) => Ok(Some(config)),
                Err(e) => Err(StepError::Retry(format!(
                    "Failed to deserialize add-party config: {e:#}"
                ))),
            },
        }
    }

    /// Sign the proposal pair as this workflow.
    async fn sign_proposals(
        self,
        node_config: &NodeConfig,
        db: &SqlitePool,
        instance_name: &str,
        proposal_data: &[u8],
    ) -> Result {
        match self {
            Self::Kick => kick::sign_proposals(node_config, db, instance_name, proposal_data).await,
            Self::ChangeThreshold => {
                change_threshold::sign_proposals(node_config, db, instance_name, proposal_data)
                    .await
            }
            Self::AddParty => {
                add_party::sign_proposals(node_config, db, instance_name, proposal_data).await
            }
        }
    }

    /// Send this node's signed pair to the coordinator as this workflow.
    async fn send_signatures(
        self,
        client: &NoiseClient,
        db: &SqlitePool,
        instance_name: &str,
        node_config: &NodeConfig,
    ) -> Result {
        match self {
            Self::Kick => {
                kick::peer::send_kick_signatures_to_coordinator(
                    client,
                    db,
                    instance_name,
                    node_config,
                )
                .await
            }
            Self::ChangeThreshold => {
                change_threshold::peer::send_change_threshold_signatures_to_coordinator(
                    client,
                    db,
                    instance_name,
                    node_config,
                )
                .await
            }
            Self::AddParty => {
                add_party::peer::send_add_party_signatures_to_coordinator(
                    client,
                    db,
                    instance_name,
                    node_config,
                )
                .await
            }
        }
    }
}
