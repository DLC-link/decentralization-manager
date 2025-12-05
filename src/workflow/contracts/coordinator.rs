use std::{collections::HashSet, sync::Arc};

use anyhow::Context;

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::SUBMISSION_SIGNATURES_PREFIX,
    dirs::WorkflowDirs,
    error::Result,
    noise::server::NoiseServer,
    workflow::state::WorkflowState,
};

use super::{
    steps::{execute_submissions, prepare_submissions, sign_submissions, upload_dars},
    ContractsStep,
};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
) -> Result {
    tracing::info!("Initializing Noise server...");

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        ContractsStep::WaitingForAttestors,
    )
    .await?;
    let server = Arc::new(server);

    let dirs = WorkflowDirs::new();
    dirs.create_required_dirs().await?;

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
    workflow_state: Arc<WorkflowState<ContractsStep>>,
    node_config: NodeConfig,
    network_config: NetworkConfig,
    dirs: WorkflowDirs,
) -> Result {
    let mut coordinator_completed_steps = HashSet::new();

    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            ContractsStep::WaitingForAttestors => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::UploadDars => {
                if !coordinator_completed_steps.contains(&ContractsStep::UploadDars) {
                    tracing::info!("Coordinator executing: Upload DARs");
                    upload_dars(&node_config, &dirs).await?;
                    coordinator_completed_steps.insert(ContractsStep::UploadDars);
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::PrepareSubmissions => {
                tracing::info!("Coordinator executing: Prepare submissions");
                prepare_submissions(&node_config, &dirs, &network_config).await?;
                workflow_state.advance_step().await;
            }
            ContractsStep::SignSubmissions => {
                tracing::info!("Coordinator executing: Sign submissions");
                sign_submissions(&node_config, &dirs)
                    .await
                    .context("Failed to sign submissions")?;
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::ExecuteSubmissions => {
                tracing::info!("Coordinator executing: Execute submissions");
                crate::workflow::save_attestor_data(
                    &workflow_state,
                    &dirs.workflow_dir,
                    SUBMISSION_SIGNATURES_PREFIX,
                )
                .await?;
                execute_submissions(&node_config, &dirs, &network_config).await?;
                workflow_state.advance_step().await;
            }
            ContractsStep::Complete => {
                tracing::info!("Contracts workflow complete!");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}
