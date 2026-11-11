use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use tokio::time::Duration;

use super::{
    ARTIFACTS, KickConfig, KickStep,
    steps::{create_proposals, export_state, prune_cached_membership, submit_kick},
};
use crate::{
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        CoordinatorSteps, StepFlow, run_coordinator,
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds},
        topology::{retire_legacy_keys_and_wait, store_signed_pairs},
    },
};

/// Run the kick workflow as the coordinator until it completes or fails.
///
/// # Errors
///
/// Returns an error if the Noise key file cannot be loaded. Returns an error
/// if a kick step fails: a Canton Admin API call, an artefact read or write,
/// or a proposal check. Returns an error if the peers it still waits on give
/// up or go silent, or if the workflow task panics. A failed prune of the
/// cached membership is only logged.
pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    kick_config: KickConfig,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result {
    tracing::info!("Initializing Noise server...");

    // Exclude the participant being kicked from peers
    let excluded_participants = kick_config
        .participant_id
        .iter()
        .map(ToString::to_string)
        .collect();

    let server = NoiseServer::new(
        node_config.clone(),
        network_config,
        db.clone(),
        kick_config.instance_name.clone(),
        KickStep::WaitingForPeers,
        Some(excluded_participants),
        last_seen,
    )
    .await?;

    let steps = KickSteps {
        instance_name: kick_config.instance_name.clone(),
        node_config,
        kick_config,
        db,
        completed_steps: HashSet::new(),
    };
    run_coordinator(server, ActiveWorkflow::Kick, instance, async { Ok(steps) }).await
}

/// The kick coordinator's work at each step.
struct KickSteps {
    node_config: NodeConfig,
    kick_config: KickConfig,
    db: sqlx::SqlitePool,
    instance_name: String,
    /// The steps this node has run that it must not run again.
    completed_steps: HashSet<KickStep>,
}

impl CoordinatorSteps<KickStep> for KickSteps {
    async fn run_step(
        &mut self,
        workflow_state: &WorkflowState<KickStep>,
        current_step: KickStep,
    ) -> Result<StepFlow> {
        tracing::debug!("Coordinator in step: {current_step:?}");

        match current_step {
            KickStep::WaitingForPeers => Ok(StepFlow::Idle),
            KickStep::ExportState => {
                if !self.completed_steps.contains(&KickStep::ExportState) {
                    tracing::info!("Coordinator executing: Export state");
                    export_state(
                        &self.node_config,
                        &self.db,
                        &self.instance_name,
                        &self.kick_config,
                    )
                    .await?;
                    self.completed_steps.insert(KickStep::ExportState);
                    workflow_state.advance_step().await;
                }
                Ok(StepFlow::Idle)
            }
            KickStep::CreateProposals => {
                tracing::info!("Coordinator executing: Create proposals");
                create_proposals(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &self.kick_config,
                )
                .await?;

                // Load kick proposals to send to peers with SignKick command.
                // Combine kick config + DNS and P2P kick proposals into a single
                // length-prefixed payload (decoded as 3 items by the peer).
                let dns_data = self
                    .db
                    .read_artifact(&self.instance_name, artifact_kinds::KICK_DNS_PROPOSAL, None)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("KICK_DNS_PROPOSAL artifact missing after CreateProposals")
                    })?;
                let p2p_data = self
                    .db
                    .read_artifact(&self.instance_name, artifact_kinds::KICK_P2P_PROPOSAL, None)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("KICK_P2P_PROPOSAL artifact missing after CreateProposals")
                    })?;

                let config_data = serde_json::to_vec(&self.kick_config)
                    .context("Failed to serialize kick config")?;

                let payload = utils::encode_length_prefixed(&[&config_data, &dns_data, &p2p_data]);
                workflow_state.set_command_payload(payload).await;
                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            KickStep::SignProposals => Ok(StepFlow::Idle),
            KickStep::SubmitKick => {
                tracing::info!("Coordinator executing: Submit kick");

                // Each peer sent us a single buffer containing two
                // length-prefixed protobufs (DNS sig followed by P2P sig).
                // Split that pair back into separate per-peer artefacts so
                // submit_kick can `list_artifacts(SIGNED_KICK_DNS)` and
                // `list_artifacts(SIGNED_KICK_P2P)` and join by peer id.
                let peer_data = workflow_state.get_all_peer_data().await;
                store_signed_pairs(&self.db, &self.instance_name, &peer_data, ARTIFACTS, "kick")
                    .await?;
                workflow_state.clear_peer_data().await;

                submit_kick(&self.node_config, &self.db, &self.instance_name).await?;
                // Deliberately not fatal. The kick is durable at this
                // point, so failing the step would leave the workflow
                // on SubmitKick and a resume would resubmit the same
                // topology transactions. A stale cache is recoverable
                // by the next /decentralized-parties refresh; a
                // rewound kick is not.
                if let Err(e) = prune_cached_membership(
                    &self.db,
                    &self.kick_config.decentralized_party_id,
                    self.kick_config.participant_id.as_ref(),
                    &self.kick_config.namespace_fingerprint,
                )
                .await
                {
                    tracing::warn!(
                        "Kick submitted but pruning owner key {fingerprint} ({participant_id:?}) from cached \
                         membership failed: {e}. Refresh /decentralized-parties before \
                         re-adding it.",
                        fingerprint = self.kick_config.namespace_fingerprint,
                        participant_id = self.kick_config.participant_id
                    );
                }
                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            KickStep::RetireLegacyKeys => {
                retire_legacy_keys_and_wait(
                    &self.node_config,
                    &self.kick_config.decentralized_party_id,
                )
                .await?;
                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            KickStep::Complete => {
                tracing::info!("Kick workflow complete!");
                tracing::debug!("Waiting for peers to receive Disconnect command...");
                Ok(StepFlow::Finish(Duration::from_secs(2)))
            }
        }
    }
}
