use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use tokio::time::Duration;

use super::{
    ARTIFACTS, ChangeThresholdConfig, ChangeThresholdStep,
    steps::{create_proposals, export_state, submit_change},
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

/// Run the change-threshold workflow on this node as its coordinator, until the
/// run completes or fails.
///
/// # Errors
///
/// Returns an error if the Noise key file cannot be read. Returns an error if a
/// step fails, for example a Canton Admin API call or a read or write of the
/// run's stored artefacts. Returns an error if the requested thresholds do not
/// fit the live topology. Returns an error if the peers the run waits on give
/// up or go silent, or if the workflow task panics.
pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    change_config: ChangeThresholdConfig,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result {
    tracing::info!("Initializing Noise server...");

    // Every remaining party member signs — no exclusions.
    let server = NoiseServer::new(
        node_config.clone(),
        network_config,
        db.clone(),
        change_config.instance_name.clone(),
        ChangeThresholdStep::WaitingForPeers,
        None,
        last_seen,
    )
    .await?;

    let steps = ChangeThresholdSteps {
        instance_name: change_config.instance_name.clone(),
        node_config,
        change_config,
        db,
        completed_steps: HashSet::new(),
    };
    run_coordinator(server, ActiveWorkflow::ChangeThreshold, instance, async {
        Ok(steps)
    })
    .await
}

/// The change-threshold coordinator's work at each step.
struct ChangeThresholdSteps {
    node_config: NodeConfig,
    change_config: ChangeThresholdConfig,
    db: sqlx::SqlitePool,
    instance_name: String,
    /// The steps this node has run that it must not run again.
    completed_steps: HashSet<ChangeThresholdStep>,
}

impl CoordinatorSteps<ChangeThresholdStep> for ChangeThresholdSteps {
    async fn run_step(
        &mut self,
        workflow_state: &WorkflowState<ChangeThresholdStep>,
        current_step: ChangeThresholdStep,
    ) -> Result<StepFlow> {
        tracing::debug!("Coordinator in step: {current_step:?}");

        match current_step {
            ChangeThresholdStep::WaitingForPeers => Ok(StepFlow::Idle),
            ChangeThresholdStep::ExportState => {
                if !self
                    .completed_steps
                    .contains(&ChangeThresholdStep::ExportState)
                {
                    tracing::info!("Coordinator executing: Export state");
                    export_state(
                        &self.node_config,
                        &self.db,
                        &self.instance_name,
                        &self.change_config,
                    )
                    .await?;
                    self.completed_steps
                        .insert(ChangeThresholdStep::ExportState);
                    workflow_state.advance_step().await;
                }
                Ok(StepFlow::Idle)
            }
            ChangeThresholdStep::CreateProposals => {
                tracing::info!("Coordinator executing: Create proposals");
                create_proposals(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &self.change_config,
                )
                .await?;

                // Load proposals to send to peers with the
                // SignChangeThreshold command. Combine config + DNS +
                // P2P into a single length-prefixed payload (decoded as
                // 3 items by the peer).
                let dns_data = self
                    .db
                    .read_artifact(
                        &self.instance_name,
                        artifact_kinds::CHANGE_THRESHOLD_DNS_PROPOSAL,
                        None,
                    )
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "CHANGE_THRESHOLD_DNS_PROPOSAL artifact missing after CreateProposals"
                        )
                    })?;
                let p2p_data = self
                    .db
                    .read_artifact(
                        &self.instance_name,
                        artifact_kinds::CHANGE_THRESHOLD_P2P_PROPOSAL,
                        None,
                    )
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "CHANGE_THRESHOLD_P2P_PROPOSAL artifact missing after CreateProposals"
                        )
                    })?;

                let config_data = serde_json::to_vec(&self.change_config)
                    .context("Failed to serialize change-threshold config")?;

                let payload = utils::encode_length_prefixed(&[&config_data, &dns_data, &p2p_data]);
                workflow_state.set_command_payload(payload).await;
                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            ChangeThresholdStep::SignProposals => Ok(StepFlow::Idle),
            ChangeThresholdStep::Submit => {
                tracing::info!("Coordinator executing: Submit change-threshold");

                // Each peer sent a single buffer of two length-prefixed
                // protobufs (DNS sig then P2P sig). Split that pair back
                // into per-peer artefacts so submit can list them by
                // kind and join by peer id. The kick splitter is
                // format-agnostic and shared for this.
                let peer_data = workflow_state.get_all_peer_data().await;
                store_signed_pairs(
                    &self.db,
                    &self.instance_name,
                    &peer_data,
                    ARTIFACTS,
                    "change-threshold",
                )
                .await?;
                workflow_state.clear_peer_data().await;

                submit_change(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &self.change_config,
                )
                .await?;
                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            ChangeThresholdStep::RetireLegacyKeys => {
                retire_legacy_keys_and_wait(
                    &self.node_config,
                    &self.change_config.decentralized_party_id,
                )
                .await?;
                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            ChangeThresholdStep::Complete => {
                tracing::info!("Change-threshold workflow complete!");
                tracing::debug!("Waiting for peers to receive Disconnect command...");
                Ok(StepFlow::Finish(Duration::from_secs(2)))
            }
        }
    }
}
