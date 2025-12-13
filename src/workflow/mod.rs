pub mod contracts;
pub mod kick;
pub mod onboarding;
pub mod state;

use std::sync::Arc;

use anyhow::Context;

use crate::{
    config::{CoordinatorStrategy, NetworkConfig, NodeConfig},
    consts::{LEDGER_SUBMISSIONS_DIR, PREPARED_DIR},
    error::Result,
    noise::{MessageType, client::NoiseClient, election, server::NoiseServer},
    utils,
};

pub use contracts::{ContractsConfig, ContractsStep};
pub use kick::{KickConfig, KickStep};
pub use onboarding::OnboardingStep;
pub use state::WorkflowState;

#[derive(Clone, Copy, Debug)]
pub enum WorkflowType {
    Onboarding,
    Contracts,
    Kick,
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
                "Election complete: {id} is the coordinator",
                id = election_result.coordinator.id
            );

            Ok(election_result.is_me)
        }
        _ => network_config.is_coordinator(&node_config.node.node_id),
    }
}

pub async fn start_node(
    node_config: NodeConfig,
    workflow_type: WorkflowType,
    kick_config: Option<KickConfig>,
    contracts_config: Option<ContractsConfig>,
) -> Result {
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
                onboarding::coordinator::start_coordinator(node_config, network_config).await
            }
            WorkflowType::Contracts => {
                let config = contracts_config.ok_or_else(|| {
                    anyhow::anyhow!("ContractsConfig is required for Contracts workflow")
                })?;
                contracts::coordinator::start_coordinator(node_config, network_config, config)
                    .await
            }
            WorkflowType::Kick => {
                let config = kick_config
                    .ok_or_else(|| anyhow::anyhow!("KickConfig is required for Kick workflow"))?;
                kick::coordinator::start_coordinator(node_config, network_config, config).await
            }
        }
    } else {
        start_attestor(node_config, network_config).await
    }
}

pub async fn run_server_with_workflow<S: state::WorkflowStep + 'static>(
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

pub async fn save_attestor_data<S: state::WorkflowStep + 'static>(
    workflow_state: &WorkflowState<S>,
    dir: &std::path::Path,
    prefix: &str,
) -> Result {
    let attestor_data = workflow_state.get_all_attestor_data().await;
    for (attestor_id, data) in attestor_data {
        let file_path = dir.join(format!("{prefix}-{attestor_id}.bin"));
        tokio::fs::write(&file_path, &data)
            .await
            .with_context(|| format!("Failed to write attestor data to '{}'", file_path.display()))?;
    }
    workflow_state.clear_attestor_data().await;
    Ok(())
}

/// Start node in attestor mode (client)
async fn start_attestor(node_config: NodeConfig, network_config: NetworkConfig) -> Result {
    tracing::info!("Initializing Noise client...");

    let client = NoiseClient::new(node_config.clone(), network_config.clone()).await?;

    // Initialize directory paths for all workflows
    let onboarding_dirs = onboarding::OnboardingDirs::with_base(node_config.workflow_data_dir());
    onboarding_dirs.create_dirs().await?;

    let contracts_dirs = contracts::ContractsDirs::with_base(
        node_config.workflow_data_dir(),
        node_config.dars_dir(),
    );
    contracts_dirs.create_dirs().await?;

    let kick_dirs = kick::KickDirs::with_base(node_config.workflow_data_dir());
    kick_dirs.create_dirs().await?;

    tracing::info!("Noise client initialized, entering command polling loop");

    // Command polling loop
    let mut consecutive_errors = 0;
    loop {
        // Poll coordinator for next command (with payload for commands that need data)
        let message = match client.get_next_command_with_payload().await {
            Ok(msg) => {
                consecutive_errors = 0; // Reset error count on success
                msg
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

        let command = message.msg_type;
        let payload = message.payload;
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

                // If payload contains DARs from coordinator, save them first
                if !payload.is_empty() {
                    if let Err(e) = save_dars_from_payload(&payload, &contracts_dirs).await {
                        tracing::error!("Failed to save DARs from coordinator: {e}");
                        continue;
                    }
                }

                if let Err(e) = contracts::upload_dars(&node_config, &contracts_dirs).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = client.send_status(b"UploadDars completed".to_vec()).await {
                    tracing::error!("Failed to send completion status: {e}");
                }
            }
            MessageType::GenerateKeys => {
                tracing::info!("Executing: Generate keys");
                if let Err(e) =
                    onboarding::generate_keys(&node_config, &onboarding_dirs, &network_config).await
                {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) =
                    onboarding::attestor::send_keys_to_coordinator(&client, &onboarding_dirs).await
                {
                    tracing::error!("Failed to send keys to coordinator: {e}");
                }
            }
            MessageType::SignDns => {
                tracing::info!("Executing: Sign DNS proposal");
                // Payload contains the DNS proposal from coordinator
                if payload.is_empty() {
                    tracing::error!("No DNS proposal payload received from coordinator");
                    continue;
                }
                if let Err(e) =
                    onboarding::sign_dns_proposals(&node_config, &onboarding_dirs, &payload).await
                {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = onboarding::attestor::send_dns_signature_to_coordinator(
                    &client,
                    &onboarding_dirs,
                )
                .await
                {
                    tracing::error!("Failed to send DNS signature to coordinator: {e}");
                }
            }
            MessageType::SignP2p => {
                tracing::info!("Executing: Sign P2P proposals");
                // Payload contains the P2P proposal from coordinator
                if payload.is_empty() {
                    tracing::error!("No P2P proposal payload received from coordinator");
                    continue;
                }
                if let Err(e) =
                    onboarding::sign_p2p_proposals(&node_config, &onboarding_dirs, &payload).await
                {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = onboarding::attestor::send_p2p_signatures_to_coordinator(
                    &client,
                    &onboarding_dirs,
                )
                .await
                {
                    tracing::error!("Failed to send P2P signatures to coordinator: {e}");
                }
            }
            MessageType::SignSubmissions => {
                tracing::info!("Executing: Sign submissions");

                // If payload contains prepared submissions from coordinator, save them first
                if !payload.is_empty() {
                    if let Err(e) =
                        save_prepared_submissions_from_payload(&payload, &contracts_dirs).await
                    {
                        tracing::error!("Failed to save prepared submissions from coordinator: {e}");
                        continue;
                    }
                }

                if let Err(e) = contracts::sign_submissions(&node_config, &contracts_dirs).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = contracts::attestor::send_submission_signatures_to_coordinator(
                    &client,
                    &contracts_dirs,
                )
                .await
                {
                    tracing::error!("Failed to send submission signatures to coordinator: {e}");
                }
            }
            MessageType::SignKick => {
                tracing::info!("Executing: Sign kick proposals");
                // Payload contains both DNS and P2P kick proposals from coordinator
                if payload.is_empty() {
                    tracing::error!("No kick proposals payload received from coordinator");
                    continue;
                }
                if let Err(e) = kick::sign_proposals(&node_config, &kick_dirs, &payload).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) =
                    kick::attestor::send_kick_signatures_to_coordinator(&client, &kick_dirs).await
                {
                    tracing::error!("Failed to send kick signatures to coordinator: {e}");
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
pub async fn find_and_read_file(
    dir: &std::path::Path,
    prefix: &str,
    suffix: &str,
    error_msg: &str,
) -> Result<Vec<u8>> {
    let files = utils::find_files_by_pattern(dir, prefix, suffix).await?;

    if let Some(path) = files.first() {
        let data = tokio::fs::read(path)
            .await
            .with_context(|| format!("Failed to read file '{}'", path.display()))?;
        return Ok(data);
    }

    anyhow::bail!("{error_msg} in {path}", path = dir.display())
}

/// Save DAR files received from coordinator to the dars directory
async fn save_dars_from_payload(
    payload: &[u8],
    dirs: &contracts::ContractsDirs,
) -> Result {
    let dar_files = utils::decode_files(payload)?;

    tracing::info!(
        "Received {count} DAR file(s) from coordinator",
        count = dar_files.len()
    );

    // Ensure dars directory exists
    utils::create_directory(&dirs.dars_dir).await?;

    for (filename, data) in dar_files {
        let path = dirs.dars_dir.join(&filename);
        tokio::fs::write(&path, &data).await?;
        tracing::debug!("Saved DAR: {filename}");
    }

    Ok(())
}

/// Save prepared submission files received from coordinator
async fn save_prepared_submissions_from_payload(
    payload: &[u8],
    dirs: &contracts::ContractsDirs,
) -> Result {
    let files = utils::decode_files(payload)?;

    tracing::info!(
        "Received {count} prepared submission file(s) from coordinator",
        count = files.len()
    );

    let prepared_dir = dirs.workflow_dir.join(LEDGER_SUBMISSIONS_DIR).join(PREPARED_DIR);
    utils::create_directory(&prepared_dir).await?;

    for (filename, data) in files {
        let path = prepared_dir.join(&filename);
        tokio::fs::write(&path, &data).await?;
        tracing::debug!("Saved prepared submission: {filename}");
    }

    Ok(())
}
