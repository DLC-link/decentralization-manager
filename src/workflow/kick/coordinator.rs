use std::{collections::HashSet, sync::Arc};

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::SIGNED_KICK_PROPOSALS_PREFIX,
    error::Result,
    noise::server::NoiseServer,
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

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        KickStep::WaitingForAttestors,
    )
    .await?;

    let workflow_state = server.get_workflow_state();
    let server = Arc::new(server);

    let dirs = KickDirs::new();
    dirs.create_dirs().await?;

    let coordinator_workflow = {
        let workflow_state = workflow_state.clone();
        let node_config = node_config.clone();
        let network_config = network_config.clone();
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
                            export_state(&node_config, &dirs, &network_config, &kick_config).await?;
                            coordinator_completed_steps.insert(KickStep::ExportState);
                            workflow_state.advance_step().await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::CreateProposals => {
                        tracing::info!("Coordinator executing: Create proposals");
                        create_proposals(&node_config, &dirs, &network_config, &kick_config).await?;
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
                        submit_kick(&node_config, &dirs, &network_config).await?;
                        workflow_state.advance_step().await;
                    }
                    KickStep::Complete => {
                        tracing::info!("Kick workflow complete!");
                        break;
                    }
                }
            }

            Ok(())
        })
    };

    crate::workflow::run_server_with_workflow(server, coordinator_workflow).await
}
