pub mod state;
pub mod steps;

use std::{collections::HashSet, sync::Arc};

use anyhow::Context;

use crate::{
    config::{CoordinatorStrategy, NetworkConfig, NodeConfig},
    consts::{
        ATTESTOR_KEYS_PREFIX, EXECUTION_DIR, SIGNATURES_DIR, SIGNED_DNS_PROPOSAL_PREFIX,
        SIGNED_P2P_PROPOSALS_PREFIX, SUBMISSION_SIGNATURES_PREFIX,
    },
    dirs::WorkflowDirs,
    error::Result,
    noise::{MessageType, client::NoiseClient, election, server::NoiseServer},
    utils,
};

pub use state::{ContractsStep, OnboardingStep, WorkflowState};

#[derive(Debug, Clone, Copy)]
pub enum WorkflowType {
    Onboarding,
    Contracts,
}

async fn determine_coordinator(
    node_config: &NodeConfig,
    network_config: &NetworkConfig,
) -> Result<bool> {
    match network_config.network.coordinator_strategy {
        CoordinatorStrategy::Election => {
            tracing::info!("Running leader election (Bully algorithm)");
            let election_result =
                election::run_election(network_config, &node_config.node.node_id).await?;

            tracing::info!(
                "Election complete: {} is the coordinator",
                election_result.coordinator.id
            );

            Ok(election_result.is_me)
        }
        _ => network_config.is_coordinator(&node_config.node.node_id),
    }
}

pub async fn start_node(node_config: NodeConfig, workflow_type: WorkflowType) -> Result {
    tracing::info!("Loading network config...");
    let network_config = node_config.load_network_config().await?;

    let is_coordinator = determine_coordinator(&node_config, &network_config).await?;
    let role = if is_coordinator {
        "COORDINATOR"
    } else {
        "ATTESTOR"
    };
    tracing::info!("Starting {workflow_type:?} workflow as {role}");

    if is_coordinator {
        match workflow_type {
            WorkflowType::Onboarding => {
                start_onboarding_coordinator(node_config, network_config).await
            }
            WorkflowType::Contracts => {
                start_contracts_coordinator(node_config, network_config).await
            }
        }
    } else {
        start_attestor(node_config, network_config).await
    }
}

async fn start_onboarding_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
) -> Result {
    tracing::info!("Initializing Noise server...");

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        OnboardingStep::WaitingForAttestors,
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
        run_onboarding_workflow(
            workflow_state,
            node_config_clone,
            network_config_clone,
            dirs_clone,
        )
        .await
    });

    run_server_with_workflow(server, workflow_handle).await
}

async fn start_contracts_coordinator(
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
        run_contracts_workflow(
            workflow_state,
            node_config_clone,
            network_config_clone,
            dirs_clone,
        )
        .await
    });

    run_server_with_workflow(server, workflow_handle).await
}

async fn run_server_with_workflow<S: state::WorkflowStep + 'static>(
    server: Arc<NoiseServer<S>>,
    workflow_handle: tokio::task::JoinHandle<Result>,
) -> Result {
    tokio::select! {
        result = server.start() => {
            result?;
        }
        result = workflow_handle => {
            match result {
                Ok(Ok(())) => {
                    tracing::info!("Workflow completed successfully, shutting down");
                }
                Ok(Err(e)) => {
                    tracing::error!("Workflow failed: {e}");
                    anyhow::bail!("Coordinator workflow failed: {e}");
                }
                Err(e) => {
                    tracing::error!("Workflow task panicked: {e}");
                    anyhow::bail!("Coordinator workflow task panicked: {e}");
                }
            }
        }
    }
    Ok(())
}

async fn run_onboarding_workflow(
    workflow_state: Arc<WorkflowState<OnboardingStep>>,
    node_config: NodeConfig,
    network_config: NetworkConfig,
    dirs: WorkflowDirs,
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
                    steps::generate_keys(&node_config, &dirs, &network_config).await?;
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
                steps::create_proposals(&node_config, &dirs, &network_config).await?;
                workflow_state.advance_step().await;
            }
            OnboardingStep::SignDns => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::SubmitDns => {
                tracing::info!("Coordinator executing: Submit DNS proposals");
                save_attestor_data(
                    &workflow_state,
                    &dirs.dns_signed_dir,
                    SIGNED_DNS_PROPOSAL_PREFIX,
                )
                .await?;
                steps::submit_dns_proposals(&node_config, &dirs).await?;
                workflow_state.advance_step().await;
            }
            OnboardingStep::SignP2p => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::SubmitFinal => {
                tracing::info!("Coordinator executing: Submit final proposals");
                save_attestor_data(
                    &workflow_state,
                    &dirs.final_signed_dir,
                    SIGNED_P2P_PROPOSALS_PREFIX,
                )
                .await?;
                steps::submit_final_proposals(&node_config, &dirs, &network_config).await?;
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

async fn run_contracts_workflow(
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
                    steps::upload_dars(&node_config, &dirs).await?;
                    coordinator_completed_steps.insert(ContractsStep::UploadDars);
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::PrepareSubmissions => {
                tracing::info!("Coordinator executing: Prepare submissions");
                steps::prepare_submissions(&node_config, &dirs, &network_config).await?;
                workflow_state.advance_step().await;
            }
            ContractsStep::SignSubmissions => {
                tracing::info!("Coordinator executing: Sign submissions");
                steps::sign_submissions(&node_config, &dirs)
                    .await
                    .context("Failed to sign submissions")?;
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::ExecuteSubmissions => {
                tracing::info!("Coordinator executing: Execute submissions");
                save_attestor_data(
                    &workflow_state,
                    &dirs.workflow_dir,
                    SUBMISSION_SIGNATURES_PREFIX,
                )
                .await?;
                steps::execute_submissions(&node_config, &dirs, &network_config).await?;
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

async fn save_attestor_data<S: state::WorkflowStep + 'static>(
    workflow_state: &WorkflowState<S>,
    dir: &std::path::Path,
    prefix: &str,
) -> Result<()> {
    let attestor_data = workflow_state.get_all_attestor_data().await;
    for (attestor_id, data) in attestor_data {
        let file_path = dir.join(format!("{prefix}-{attestor_id}.bin"));
        tokio::fs::write(&file_path, data).await?;
    }
    workflow_state.clear_attestor_data().await;
    Ok(())
}

/// Start node in attestor mode (client)
async fn start_attestor(node_config: NodeConfig, network_config: NetworkConfig) -> Result {
    tracing::info!("Initializing Noise client...");

    let client = NoiseClient::new(node_config.clone(), network_config.clone()).await?;

    // Initialize directory paths
    let dirs = WorkflowDirs::new();
    dirs.create_required_dirs().await?;

    tracing::info!("Noise client initialized, entering command polling loop");

    // Command polling loop
    let mut consecutive_errors = 0;
    loop {
        // Poll coordinator for next command
        let command = match client.get_next_command().await {
            Ok(cmd) => {
                consecutive_errors = 0; // Reset error count on success
                cmd
            }
            Err(e) => {
                consecutive_errors += 1;
                tracing::error!("Failed to get next command: {e}");

                // If we get multiple connection refused errors in a row,
                // the coordinator has likely shut down or there's a persistent error
                if consecutive_errors >= 3 {
                    tracing::error!(
                        "Failed to communicate with coordinator after 3 attempts. Aborting."
                    );
                    anyhow::bail!(
                        "Attestor failed: persistent communication errors with coordinator"
                    );
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        tracing::info!("Received command: {command:?}");

        match command {
            MessageType::Wait => {
                // Coordinator says to wait, poll again after delay
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            }
            MessageType::Disconnect => {
                tracing::info!("Received disconnect command, shutting down");
                break;
            }
            MessageType::UploadDars => {
                tracing::info!("Executing: Upload DARs");
                if let Err(e) = steps::upload_dars(&node_config, &dirs).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = client.send_status(b"UploadDars completed".to_vec()).await {
                    tracing::error!("Failed to send completion status: {e}");
                }
            }
            MessageType::GenerateKeys => {
                tracing::info!("Executing: Generate keys");
                if let Err(e) = steps::generate_keys(&node_config, &dirs, &network_config).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = send_keys_to_coordinator(&client, &dirs).await {
                    tracing::error!("Failed to send keys to coordinator: {e}");
                }
            }
            MessageType::SignDns => {
                tracing::info!("Executing: Sign DNS proposal");
                if let Err(e) = steps::sign_dns_proposals(&node_config, &dirs).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = send_dns_signature_to_coordinator(&client, &dirs).await {
                    tracing::error!("Failed to send DNS signature to coordinator: {e}");
                }
            }
            MessageType::SignP2p => {
                tracing::info!("Executing: Sign P2P proposals");
                if let Err(e) = steps::sign_p2p_proposals(&node_config, &dirs).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = send_p2p_signatures_to_coordinator(&client, &dirs).await {
                    tracing::error!("Failed to send P2P signatures to coordinator: {e}");
                }
            }
            MessageType::SignSubmissions => {
                tracing::info!("Executing: Sign submissions");
                if let Err(e) = steps::sign_submissions(&node_config, &dirs).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = send_submission_signatures_to_coordinator(&client, &dirs).await {
                    tracing::error!("Failed to send submission signatures to coordinator: {e}");
                }
            }
            _ => {
                tracing::warn!("Unexpected message type: {command:?}");
            }
        }
    }

    tracing::info!("Attestor shutting down");
    Ok(())
}

/// Find and read the first file matching prefix/suffix pattern
async fn find_and_read_file(
    dir: &std::path::Path,
    prefix: &str,
    suffix: &str,
    error_msg: &str,
) -> Result<Vec<u8>> {
    let files = utils::find_files_by_pattern(dir, prefix, suffix).await?;

    if let Some(path) = files.first() {
        let data = tokio::fs::read(path).await?;
        return Ok(data);
    }

    anyhow::bail!("{} in {}", error_msg, dir.display())
}

/// Send generated keys to coordinator
async fn send_keys_to_coordinator(client: &NoiseClient, dirs: &WorkflowDirs) -> Result {
    let data = find_and_read_file(
        &dirs.keys_dir,
        ATTESTOR_KEYS_PREFIX,
        ".bin",
        "Attestor public keys file not found",
    )
    .await?;
    client.upload_keys(data).await?;
    Ok(())
}

/// Send DNS signature to coordinator
async fn send_dns_signature_to_coordinator(client: &NoiseClient, dirs: &WorkflowDirs) -> Result {
    let data = find_and_read_file(
        &dirs.dns_signed_dir,
        SIGNED_DNS_PROPOSAL_PREFIX,
        ".bin",
        "Signed DNS proposal file not found",
    )
    .await?;
    client.send_dns_signature(data).await?;
    Ok(())
}

/// Send P2P signatures to coordinator
async fn send_p2p_signatures_to_coordinator(client: &NoiseClient, dirs: &WorkflowDirs) -> Result {
    let data = find_and_read_file(
        &dirs.final_signed_dir,
        SIGNED_P2P_PROPOSALS_PREFIX,
        ".bin",
        "Signed P2P proposals file not found",
    )
    .await?;
    client.send_p2p_signatures(data).await?;
    Ok(())
}

/// Send submission signatures to coordinator
async fn send_submission_signatures_to_coordinator(
    client: &NoiseClient,
    dirs: &WorkflowDirs,
) -> Result {
    let signatures_dir = dirs.workflow_dir.join(EXECUTION_DIR).join(SIGNATURES_DIR);
    let data = find_and_read_file(
        &signatures_dir,
        SUBMISSION_SIGNATURES_PREFIX,
        ".bin",
        "Submission signatures file not found",
    )
    .await?;
    client.send_submission_signatures(data).await?;
    Ok(())
}
