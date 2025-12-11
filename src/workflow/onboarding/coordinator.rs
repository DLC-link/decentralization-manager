use std::{collections::HashSet, sync::Arc};

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{SIGNED_DNS_PROPOSAL_PREFIX, SIGNED_P2P_PROPOSALS_PREFIX},
    error::Result,
    noise::server::NoiseServer,
    workflow::state::WorkflowState,
};

use super::{
    OnboardingDirs, OnboardingStep,
    steps::{create_proposals, generate_keys, submit_dns_proposals, submit_final_proposals},
};

pub async fn start_coordinator(node_config: NodeConfig, network_config: NetworkConfig) -> Result {
    tracing::info!("Initializing Noise server...");

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        OnboardingStep::WaitingForAttestors,
        None, // No excluded participants
    )
    .await?;
    let server = Arc::new(server);

    let dirs = OnboardingDirs::with_base(node_config.workflow_data_dir());
    dirs.create_dirs().await?;

    tracing::info!("Noise server initialized, listening for connections");

    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let network_config_clone = network_config.clone();
    let dirs_clone = dirs.clone();
    let workflow_handle = tokio::spawn(async move {
        run_workflow(
            workflow_state,
            node_config_clone,
            network_config_clone,
            dirs_clone,
        )
        .await
    });

    crate::workflow::run_server_with_workflow(server, workflow_handle).await
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<OnboardingStep>>,
    node_config: NodeConfig,
    network_config: NetworkConfig,
    dirs: OnboardingDirs,
) -> Result {
    let mut coordinator_completed_steps = HashSet::new();

    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            OnboardingStep::WaitingForAttestors => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::GenerateKeys => {
                if !coordinator_completed_steps.contains(&OnboardingStep::GenerateKeys) {
                    tracing::info!("Coordinator executing: Generate keys");
                    generate_keys(&node_config, &dirs, &network_config).await?;
                    coordinator_completed_steps.insert(OnboardingStep::GenerateKeys);

                    tracing::info!(
                        "Waiting 3 seconds for Canton to process namespace delegations..."
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::CreateProposals => {
                tracing::info!("Coordinator executing: Create proposals");
                create_proposals(&node_config, &dirs, &network_config).await?;
                workflow_state.advance_step().await;
            }
            OnboardingStep::SignDns => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::SubmitDns => {
                tracing::info!("Coordinator executing: Submit DNS proposals");
                crate::workflow::save_attestor_data(
                    &workflow_state,
                    &dirs.dns_signed_dir,
                    SIGNED_DNS_PROPOSAL_PREFIX,
                )
                .await?;
                submit_dns_proposals(&node_config, &dirs).await?;
                workflow_state.advance_step().await;
            }
            OnboardingStep::SignP2p => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::SubmitFinal => {
                tracing::info!("Coordinator executing: Submit final proposals");
                crate::workflow::save_attestor_data(
                    &workflow_state,
                    &dirs.final_signed_dir,
                    SIGNED_P2P_PROPOSALS_PREFIX,
                )
                .await?;
                submit_final_proposals(&node_config, &dirs, &network_config).await?;
                workflow_state.advance_step().await;
            }
            OnboardingStep::Complete => {
                tracing::info!("Onboarding workflow complete!");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}
