use std::{collections::HashSet, sync::Arc};

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{DNS_KICK_PROTO_FILENAME, P2P_KICK_PROTO_FILENAME, SIGNED_KICK_PROPOSALS_PREFIX},
    error::Result,
    noise::server::NoiseServer,
    utils,
};

use super::{
    KickConfig, KickDirs, KickStep,
    steps::{create_proposals, export_state, submit_kick},
};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    kick_config: KickConfig,
) -> Result {
    tracing::info!("Initializing Noise server...");

    // Exclude the participant being kicked from attestors
    let excluded_participants = vec![kick_config.participant_id.to_string()];

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        KickStep::WaitingForAttestors,
        Some(excluded_participants),
    )
    .await?;

    let workflow_state = server.get_workflow_state();
    let server = Arc::new(server);

    let dirs = KickDirs::with_base(node_config.workflow_data_dir());
    dirs.create_dirs().await?;

    let coordinator_workflow = {
        let workflow_state = workflow_state.clone();
        let node_config = node_config.clone();
        let kick_config = kick_config.clone();
        let dirs = dirs.clone();

        tokio::spawn(async move {
            let mut coordinator_completed_steps = HashSet::new();

            loop {
                let current_step = workflow_state.current_step().await;
                tracing::debug!("Coordinator in step: {current_step:?}");

                match current_step {
                    KickStep::WaitingForAttestors => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::ExportState => {
                        if !coordinator_completed_steps.contains(&KickStep::ExportState) {
                            tracing::info!("Coordinator executing: Export state");
                            export_state(&node_config, &dirs, &kick_config).await?;
                            coordinator_completed_steps.insert(KickStep::ExportState);
                            workflow_state.advance_step().await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::CreateProposals => {
                        tracing::info!("Coordinator executing: Create proposals");
                        create_proposals(&node_config, &dirs, &kick_config).await?;

                        // Load kick proposals to send to attestors with SignKick command
                        // Combine both DNS and P2P kick proposals into a single payload
                        let dns_kick_path = dirs.kick_proposals_dir.join(DNS_KICK_PROTO_FILENAME);
                        let p2p_kick_path = dirs.kick_proposals_dir.join(P2P_KICK_PROTO_FILENAME);

                        let dns_data = tokio::fs::read(&dns_kick_path).await?;
                        let p2p_data = tokio::fs::read(&p2p_kick_path).await?;

                        let payload = utils::encode_length_prefixed(&[&dns_data, &p2p_data]);
                        workflow_state.set_command_payload(payload).await;
                        workflow_state.advance_step().await;
                    }
                    KickStep::SignProposals => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::SubmitKick => {
                        tracing::info!("Coordinator executing: Submit kick");
                        crate::workflow::save_attestor_data(
                            &workflow_state,
                            &dirs.kick_signed_dir,
                            SIGNED_KICK_PROPOSALS_PREFIX,
                        )
                        .await?;
                        submit_kick(&node_config, &dirs).await?;
                        workflow_state.advance_step().await;
                    }
                    KickStep::Complete => {
                        tracing::info!("Kick workflow complete!");
                        tracing::debug!("Waiting for attestors to receive Disconnect command...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        break;
                    }
                }
            }

            Ok(())
        })
    };

    crate::workflow::run_server_with_workflow(server, coordinator_workflow).await
}
