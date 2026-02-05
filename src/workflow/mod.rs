pub mod add_party;
pub mod contracts;
pub mod kick;
pub mod onboarding;
pub mod state;

use std::sync::Arc;

use anyhow::Context;

use crate::{
    auth::WorkflowAuth,
    config::{NodeConfig, Peer},
    consts::{LEDGER_SUBMISSIONS_DIR, PREPARED_DIR},
    error::Result,
    noise::{MessageType, client::NoiseClient, server::NoiseServer},
    utils,
};

pub use add_party::{AddPartyConfig, AddPartyStep};
pub use contracts::{ContractsConfig, ContractsStep};
pub use kick::{KickConfig, KickStep};
pub use onboarding::{OnboardingConfig, OnboardingStep};
pub use state::WorkflowState;

/// Workflow types that can be run
#[derive(Clone, Copy, Debug)]
pub enum WorkflowType {
    Onboarding,
    Contracts,
    Kick,
    AddParty,
}

/// Start a coordinator workflow (called when this node initiates the workflow from UI)
pub async fn start_coordinator(
    node_config: NodeConfig,
    workflow_type: WorkflowType,
    onboarding_config: Option<OnboardingConfig>,
    kick_config: Option<KickConfig>,
    contracts_config: Option<ContractsConfig>,
    add_party_config: Option<AddPartyConfig>,
    workflow_auth: Option<WorkflowAuth>,
) -> Result {
    tracing::info!("Loading network config...");
    let network_config = node_config.load_network_config().await?;

    tracing::info!("Starting {workflow_type:?} workflow as COORDINATOR");

    match workflow_type {
        WorkflowType::Onboarding => {
            let config = onboarding_config.ok_or_else(|| {
                anyhow::anyhow!("OnboardingConfig is required for Onboarding workflow")
            })?;
            onboarding::coordinator::start_coordinator(node_config, network_config, config).await
        }
        WorkflowType::Contracts => {
            let config = contracts_config.ok_or_else(|| {
                anyhow::anyhow!("ContractsConfig is required for Contracts workflow")
            })?;
            contracts::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                workflow_auth,
            )
            .await
        }
        WorkflowType::Kick => {
            let config = kick_config
                .ok_or_else(|| anyhow::anyhow!("KickConfig is required for Kick workflow"))?;
            kick::coordinator::start_coordinator(node_config, network_config, config).await
        }
        WorkflowType::AddParty => {
            let config = add_party_config.ok_or_else(|| {
                anyhow::anyhow!("AddPartyConfig is required for AddParty workflow")
            })?;
            add_party::coordinator::start_coordinator(node_config, network_config, config).await
        }
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
                    tracing::error!("Workflow failed: {e:#}");
                    anyhow::bail!("Coordinator workflow failed: {e:#}");
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
        tokio::fs::write(&file_path, &data).await.with_context(|| {
            format!("Failed to write attestor data to '{}'", file_path.display())
        })?;
    }
    workflow_state.clear_attestor_data().await;
    Ok(())
}

/// Start node in attestor mode (client)
/// Called when this node receives a workflow invite from the coordinator
pub async fn start_attestor(node_config: NodeConfig, coordinator: Peer) -> Result {
    tracing::info!(
        "Initializing Noise client to connect to coordinator {}...",
        coordinator.participant_id
    );

    let client = NoiseClient::new(node_config.clone(), coordinator).await?;

    // Directories are created lazily when workflow config is received
    let mut onboarding_dirs: Option<onboarding::OnboardingDirs> = None;
    let mut _add_party_dirs: Option<add_party::AddPartyDirs> = None;

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
        tracing::debug!("Received command: {command:?}");

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

                // Decode DAR files from payload and upload directly
                let dar_files = if payload.is_empty() {
                    Vec::new()
                } else {
                    match utils::decode_files(&payload) {
                        Ok(files) => files,
                        Err(e) => {
                            tracing::error!("Failed to decode DARs from coordinator: {e}");
                            continue;
                        }
                    }
                };

                if let Err(e) = contracts::upload_dars_from_bytes(&node_config, dar_files).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = client.send_status(b"UploadDars completed".to_vec()).await {
                    tracing::error!("Failed to send completion status: {e}");
                }
            }
            MessageType::GenerateKeys => {
                tracing::info!("Executing: Generate keys");
                // Deserialize onboarding config from payload
                let onboarding_config: onboarding::OnboardingConfig =
                    match serde_json::from_slice(&payload) {
                        Ok(config) => config,
                        Err(e) => {
                            tracing::error!("Failed to deserialize onboarding config: {e}");
                            continue;
                        }
                    };

                // Create directories lazily on first command with config
                let dirs = onboarding::OnboardingDirs::with_base(
                    node_config.workflow_data_dir(),
                    &onboarding_config.instance_name,
                );
                if let Err(e) = dirs.create_dirs().await {
                    tracing::error!("Failed to create onboarding dirs: {e}");
                    continue;
                }
                onboarding_dirs = Some(dirs.clone());

                if let Err(e) =
                    onboarding::generate_keys(&node_config, &dirs, &onboarding_config).await
                {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = onboarding::attestor::send_keys_to_coordinator(&client, &dirs).await
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
                let Some(ref dirs) = onboarding_dirs else {
                    tracing::error!("Onboarding dirs not initialized (GenerateKeys not received?)");
                    continue;
                };
                if let Err(e) = onboarding::sign_dns_proposals(&node_config, dirs, &payload).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) =
                    onboarding::attestor::send_dns_signature_to_coordinator(&client, dirs).await
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
                let Some(ref dirs) = onboarding_dirs else {
                    tracing::error!("Onboarding dirs not initialized (GenerateKeys not received?)");
                    continue;
                };
                if let Err(e) = onboarding::sign_p2p_proposals(&node_config, dirs, &payload).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) =
                    onboarding::attestor::send_p2p_signatures_to_coordinator(&client, dirs).await
                {
                    tracing::error!("Failed to send P2P signatures to coordinator: {e}");
                }
            }
            MessageType::SignSubmissions => {
                tracing::info!("Executing: Sign submissions");

                if payload.is_empty() {
                    tracing::error!("No submissions payload received from coordinator");
                    continue;
                }

                // Decode config and files from payload: [config_json, files_payload]
                let items = match utils::decode_length_prefixed(&payload, 2) {
                    Ok(items) => items,
                    Err(e) => {
                        tracing::error!("Failed to decode SignSubmissions payload: {e}");
                        continue;
                    }
                };

                let contracts_config: contracts::ContractsConfig =
                    match serde_json::from_slice(&items[0]) {
                        Ok(config) => config,
                        Err(e) => {
                            tracing::error!("Failed to deserialize contracts config: {e}");
                            continue;
                        }
                    };

                // Create directories lazily on first command with config
                let dirs = contracts::ContractsDirs::with_base(
                    node_config.workflow_data_dir(),
                    &contracts_config.instance_name,
                    &contracts_config.decentralized_party_id.prefix,
                    node_config.dars_dir(),
                );
                if let Err(e) = dirs.create_dirs().await {
                    tracing::error!("Failed to create contracts dirs: {e}");
                    continue;
                }

                // Save prepared submissions from payload
                if let Err(e) = save_prepared_submissions_from_payload(&items[1], &dirs).await {
                    tracing::error!("Failed to save prepared submissions from coordinator: {e}");
                    continue;
                }

                if let Err(e) = contracts::sign_submissions(&node_config, &dirs).await {
                    tracing::error!("Step execution failed: {e:#}");
                    continue;
                }
                if let Err(e) =
                    contracts::attestor::send_submission_signatures_to_coordinator(&client, &dirs)
                        .await
                {
                    tracing::error!("Failed to send submission signatures to coordinator: {e}");
                }
            }
            MessageType::SignKick => {
                tracing::info!("Executing: Sign kick proposals");
                // Payload contains: [config_json, dns_kick_data, p2p_kick_data]
                if payload.is_empty() {
                    tracing::error!("No kick proposals payload received from coordinator");
                    continue;
                }

                // Decode config and kick data from payload
                let items = match utils::decode_length_prefixed(&payload, 3) {
                    Ok(items) => items,
                    Err(e) => {
                        tracing::error!("Failed to decode SignKick payload: {e}");
                        continue;
                    }
                };

                let kick_config: kick::KickConfig = match serde_json::from_slice(&items[0]) {
                    Ok(config) => config,
                    Err(e) => {
                        tracing::error!("Failed to deserialize kick config: {e}");
                        continue;
                    }
                };

                // Create directories lazily on first command with config
                let dirs = kick::KickDirs::with_base(
                    node_config.workflow_data_dir(),
                    &kick_config.instance_name,
                );
                if let Err(e) = dirs.create_dirs().await {
                    tracing::error!("Failed to create kick dirs: {e}");
                    continue;
                }

                // Re-encode the kick data (without config) for sign_proposals
                let kick_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
                if let Err(e) = kick::sign_proposals(&node_config, &dirs, &kick_data).await {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) =
                    kick::attestor::send_kick_signatures_to_coordinator(&client, &dirs).await
                {
                    tracing::error!("Failed to send kick signatures to coordinator: {e}");
                }
            }
            MessageType::GenerateAddPartyKeys => {
                tracing::info!("Executing: Generate add party keys (new member)");
                // Deserialize add party config from payload
                let add_party_config: add_party::AddPartyConfig =
                    match serde_json::from_slice(&payload) {
                        Ok(config) => config,
                        Err(e) => {
                            tracing::error!("Failed to deserialize add party config: {e}");
                            continue;
                        }
                    };

                // Only the new member generates keys
                if *node_config.participant_id() != add_party_config.new_participant_id {
                    tracing::info!("Not the new member, skipping key generation");
                    if let Err(e) = client
                        .send_status(b"Not new member, skipped".to_vec())
                        .await
                    {
                        tracing::error!("Failed to send status: {e}");
                    }
                    continue;
                }

                // Create directories lazily on first command with config
                let dirs = add_party::AddPartyDirs::with_base(
                    node_config.workflow_data_dir(),
                    &add_party_config.instance_name,
                );
                if let Err(e) = dirs.create_dirs().await {
                    tracing::error!("Failed to create add party dirs: {e}");
                    continue;
                }
                _add_party_dirs = Some(dirs.clone());

                if let Err(e) =
                    add_party::generate_keys(&node_config, &dirs, &add_party_config).await
                {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) = add_party::attestor::send_keys_to_coordinator(&client, &dirs).await
                {
                    tracing::error!("Failed to send keys to coordinator: {e}");
                }
            }
            MessageType::SignAddParty => {
                tracing::info!("Executing: Sign add party proposals");
                // Payload contains: [config_json, dns_add_party_data, p2p_add_party_data]
                if payload.is_empty() {
                    tracing::error!("No add party proposals payload received from coordinator");
                    continue;
                }

                // Decode config and add party data from payload
                let items = match utils::decode_length_prefixed(&payload, 3) {
                    Ok(items) => items,
                    Err(e) => {
                        tracing::error!("Failed to decode SignAddParty payload: {e}");
                        continue;
                    }
                };

                let add_party_config: add_party::AddPartyConfig =
                    match serde_json::from_slice(&items[0]) {
                        Ok(config) => config,
                        Err(e) => {
                            tracing::error!("Failed to deserialize add party config: {e}");
                            continue;
                        }
                    };

                // Both existing members AND the new member sign
                // The new member signs to prove consent to being added
                let is_new_member =
                    *node_config.participant_id() == add_party_config.new_participant_id;
                if is_new_member {
                    tracing::info!(
                        "New member signing add party proposals (consent to being added)"
                    );
                } else {
                    tracing::info!("Existing member signing add party proposals");
                }

                // Create directories lazily on first command with config
                let dirs = add_party::AddPartyDirs::with_base(
                    node_config.workflow_data_dir(),
                    &add_party_config.instance_name,
                );
                if let Err(e) = dirs.create_dirs().await {
                    tracing::error!("Failed to create add party dirs: {e}");
                    continue;
                }

                // Re-encode the add party data (without config) for sign_proposals
                let add_party_data = utils::encode_length_prefixed(&[&items[1], &items[2]]);
                if let Err(e) =
                    add_party::sign_proposals(&node_config, &dirs, &add_party_data).await
                {
                    tracing::error!("Step execution failed: {e}");
                    continue;
                }
                if let Err(e) =
                    add_party::attestor::send_add_party_signatures_to_coordinator(&client, &dirs)
                        .await
                {
                    tracing::error!("Failed to send add party signatures to coordinator: {e}");
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

    let prepared_dir = dirs
        .workflow_dir
        .join(LEDGER_SUBMISSIONS_DIR)
        .join(PREPARED_DIR);
    utils::create_directory(&prepared_dir).await?;

    for (filename, data) in files {
        let path = prepared_dir.join(&filename);
        tokio::fs::write(&path, &data).await?;
        tracing::debug!("Saved prepared submission: {filename}");
    }

    Ok(())
}
