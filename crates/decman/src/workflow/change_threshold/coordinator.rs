use std::{collections::HashSet, sync::Arc};

use anyhow::Context;

use crate::{
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        kick::coordinator::split_signed_kick_pair,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

use super::{
    ChangeThresholdConfig, ChangeThresholdStep,
    steps::{create_proposals, export_state, submit_change},
};

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
        network_config.clone(),
        db.clone(),
        change_config.instance_name.clone(),
        ChangeThresholdStep::WaitingForPeers,
        None,
        last_seen,
    )
    .await?;

    let workflow_state = server.get_workflow_state();
    let server = Arc::new(server);

    let coordinator_workflow = {
        let workflow_state = workflow_state.clone();
        let node_config = node_config.clone();
        let change_config = change_config.clone();
        let db = db.clone();
        let instance_name = change_config.instance_name.clone();

        tokio::spawn(async move {
            let mut coordinator_completed_steps = HashSet::new();

            loop {
                let current_step = workflow_state.current_step().await;
                tracing::debug!("Coordinator in step: {current_step:?}");

                match current_step {
                    ChangeThresholdStep::WaitingForPeers => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    ChangeThresholdStep::ExportState => {
                        if !coordinator_completed_steps.contains(&ChangeThresholdStep::ExportState)
                        {
                            tracing::info!("Coordinator executing: Export state");
                            export_state(&node_config, &db, &instance_name, &change_config).await?;
                            coordinator_completed_steps.insert(ChangeThresholdStep::ExportState);
                            workflow_state.advance_step().await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    ChangeThresholdStep::CreateProposals => {
                        tracing::info!("Coordinator executing: Create proposals");
                        create_proposals(&node_config, &db, &instance_name, &change_config).await?;

                        // Load proposals to send to peers with the
                        // SignChangeThreshold command. Combine config + DNS +
                        // P2P into a single length-prefixed payload (decoded as
                        // 3 items by the peer).
                        let dns_data = db
                            .read_artifact(
                                &instance_name,
                                artifact_kinds::CHANGE_THRESHOLD_DNS_PROPOSAL,
                                None,
                            )
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "CHANGE_THRESHOLD_DNS_PROPOSAL artifact missing after CreateProposals"
                                )
                            })?;
                        let p2p_data = db
                            .read_artifact(
                                &instance_name,
                                artifact_kinds::CHANGE_THRESHOLD_P2P_PROPOSAL,
                                None,
                            )
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "CHANGE_THRESHOLD_P2P_PROPOSAL artifact missing after CreateProposals"
                                )
                            })?;

                        let config_data = serde_json::to_vec(&change_config)
                            .context("Failed to serialize change-threshold config")?;

                        let payload =
                            utils::encode_length_prefixed(&[&config_data, &dns_data, &p2p_data]);
                        workflow_state.set_command_payload(payload).await;
                        workflow_state.advance_step().await;
                    }
                    ChangeThresholdStep::SignProposals => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    ChangeThresholdStep::Submit => {
                        tracing::info!("Coordinator executing: Submit change-threshold");

                        // Each peer sent a single buffer of two length-prefixed
                        // protobufs (DNS sig then P2P sig). Split that pair back
                        // into per-peer artefacts so submit can list them by
                        // kind and join by peer id. The kick splitter is
                        // format-agnostic and shared for this.
                        let peer_data = workflow_state.get_all_peer_data().await;
                        for (peer_id, combined) in &peer_data {
                            let (dns_blob, p2p_blob) = split_signed_kick_pair(combined)
                                .with_context(|| {
                                    format!(
                                        "Failed to split signed change-threshold pair from \
                                         peer {peer_id}"
                                    )
                                })?;
                            let peer_key = peer_id.to_string();
                            db.write_artifact(
                                &instance_name,
                                artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
                                Some(&peer_key),
                                &dns_blob,
                            )
                            .await?;
                            db.write_artifact(
                                &instance_name,
                                artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
                                Some(&peer_key),
                                &p2p_blob,
                            )
                            .await?;
                        }
                        workflow_state.clear_peer_data().await;

                        submit_change(&node_config, &db, &instance_name).await?;
                        workflow_state.advance_step().await;
                    }
                    ChangeThresholdStep::Complete => {
                        tracing::info!("Change-threshold workflow complete!");
                        tracing::debug!("Waiting for peers to receive Disconnect command...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        break;
                    }
                }
            }

            Ok(())
        })
    };

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::ChangeThreshold(server),
        instance,
        coordinator_workflow,
    )
    .await
}
