use std::{collections::HashSet, sync::Arc};

use anyhow::Context;

use crate::{
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        storage::{WorkflowStorage, artifact_kinds},
        topology::split_signed_kick_pair,
    },
};

use super::{
    KickConfig, KickStep,
    steps::{create_proposals, export_state, prune_cached_membership, submit_kick},
};

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
        network_config.clone(),
        db.clone(),
        kick_config.instance_name.clone(),
        KickStep::WaitingForPeers,
        Some(excluded_participants),
        last_seen,
    )
    .await?;

    let workflow_state = server.get_workflow_state();
    let server = Arc::new(server);

    let coordinator_workflow = {
        let workflow_state = workflow_state.clone();
        let node_config = node_config.clone();
        let kick_config = kick_config.clone();
        let db = db.clone();
        let instance_name = kick_config.instance_name.clone();

        tokio::spawn(async move {
            let mut coordinator_completed_steps = HashSet::new();

            loop {
                let current_step = workflow_state.current_step().await;
                tracing::debug!("Coordinator in step: {current_step:?}");

                match current_step {
                    KickStep::WaitingForPeers => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::ExportState => {
                        if !coordinator_completed_steps.contains(&KickStep::ExportState) {
                            tracing::info!("Coordinator executing: Export state");
                            export_state(&node_config, &db, &instance_name, &kick_config).await?;
                            coordinator_completed_steps.insert(KickStep::ExportState);
                            workflow_state.advance_step().await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::CreateProposals => {
                        tracing::info!("Coordinator executing: Create proposals");
                        create_proposals(&node_config, &db, &instance_name, &kick_config).await?;

                        // Load kick proposals to send to peers with SignKick command.
                        // Combine kick config + DNS and P2P kick proposals into a single
                        // length-prefixed payload (decoded as 3 items by the peer).
                        let dns_data = db
                            .read_artifact(&instance_name, artifact_kinds::KICK_DNS_PROPOSAL, None)
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "KICK_DNS_PROPOSAL artifact missing after CreateProposals"
                                )
                            })?;
                        let p2p_data = db
                            .read_artifact(&instance_name, artifact_kinds::KICK_P2P_PROPOSAL, None)
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "KICK_P2P_PROPOSAL artifact missing after CreateProposals"
                                )
                            })?;

                        let config_data = serde_json::to_vec(&kick_config)
                            .context("Failed to serialize kick config")?;

                        let payload =
                            utils::encode_length_prefixed(&[&config_data, &dns_data, &p2p_data]);
                        workflow_state.set_command_payload(payload).await;
                        workflow_state.advance_step().await;
                    }
                    KickStep::SignProposals => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::SubmitKick => {
                        tracing::info!("Coordinator executing: Submit kick");

                        // Each peer sent us a single buffer containing two
                        // length-prefixed protobufs (DNS sig followed by P2P sig).
                        // Split that pair back into separate per-peer artefacts so
                        // submit_kick can `list_artifacts(SIGNED_KICK_DNS)` and
                        // `list_artifacts(SIGNED_KICK_P2P)` and join by peer id.
                        let peer_data = workflow_state.get_all_peer_data().await;
                        for (peer_id, combined) in &peer_data {
                            let (dns_blob, p2p_blob) = split_signed_kick_pair(combined)
                                .with_context(|| {
                                    format!(
                                        "Failed to split signed kick pair from \
                                         peer {peer_id}"
                                    )
                                })?;
                            let peer_key = peer_id.to_string();
                            db.write_artifact(
                                &instance_name,
                                artifact_kinds::SIGNED_KICK_DNS,
                                Some(&peer_key),
                                &dns_blob,
                            )
                            .await?;
                            db.write_artifact(
                                &instance_name,
                                artifact_kinds::SIGNED_KICK_P2P,
                                Some(&peer_key),
                                &p2p_blob,
                            )
                            .await?;
                        }
                        workflow_state.clear_peer_data().await;

                        submit_kick(&node_config, &db, &instance_name).await?;
                        // Deliberately not fatal. The kick is durable at this
                        // point, so failing the step would leave the workflow
                        // on SubmitKick and a resume would resubmit the same
                        // topology transactions. A stale cache is recoverable
                        // by the next /decentralized-parties refresh; a
                        // rewound kick is not.
                        if let Err(e) = prune_cached_membership(
                            &db,
                            &kick_config.decentralized_party_id,
                            kick_config.participant_id.as_ref(),
                            &kick_config.namespace_fingerprint,
                        )
                        .await
                        {
                            tracing::warn!(
                                "Kick submitted but pruning owner key {} ({:?}) from cached \
                                 membership failed: {e}. Refresh /decentralized-parties before \
                                 re-adding it.",
                                kick_config.namespace_fingerprint,
                                kick_config.participant_id
                            );
                        }
                        workflow_state.advance_step().await;
                    }
                    KickStep::RetireLegacyKeys => {
                        crate::workflow::topology::retire_legacy_keys_and_wait(
                            &node_config,
                            &kick_config.decentralized_party_id,
                        )
                        .await?;
                        workflow_state.advance_step().await;
                    }
                    KickStep::Complete => {
                        tracing::info!("Kick workflow complete!");
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
        ActiveWorkflow::Kick(server),
        instance,
        coordinator_workflow,
    )
    .await
}
