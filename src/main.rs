mod cli;

use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    prelude::*,
};

use std::{path::PathBuf, sync::Arc};

use cli::{Cli, Commands, Parser};

use dec_party_onboarding::{
    config::{CoordinatorStrategy, NetworkConfig, NodeConfig},
    dirs::WorkflowDirs,
    error::Result,
    noise::{MessageType, client::NoiseClient, election, server::NoiseServer},
    steps,
    workflow_state::WorkflowState,
};

#[tokio::main]
async fn main() -> Result {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .init();

    let args = Cli::parse();

    // Handle keygen command early (doesn't require config)
    if let Commands::Keygen { ref output } = args.command {
        dec_party_onboarding::noise::generate_keypair(output).await?;
        return Ok(());
    }

    // Load configuration (required for all other commands)
    let node_config = if let Some(config_path) = &args.config {
        tracing::info!("Loading configuration from: {}", config_path.display());
        NodeConfig::from_file(config_path).await?
    } else {
        anyhow::bail!("Configuration file is required. Use -c <config-file>");
    };

    // Initialize directory paths
    let dirs = WorkflowDirs::new();
    dirs.create_required_dirs().await?;

    // Execute the requested command
    match args.command {
        Commands::Keygen { .. } => unreachable!("Keygen handled earlier"),
        Commands::Start => {
            start_node(node_config).await?;
        }
        Commands::All => steps::run_all_steps(&node_config, &dirs).await?,
        Commands::UploadDars => steps::upload_dars(&node_config, &dirs).await?,
        Commands::GenerateKeys => steps::generate_keys(&node_config, &dirs).await?,
        Commands::CreateProposals => steps::create_proposals(&node_config, &dirs).await?,
        Commands::SignDnsProposals => steps::sign_dns_proposals(&node_config, &dirs).await?,
        Commands::SubmitDnsProposals => steps::submit_dns_proposals(&node_config, &dirs).await?,
        Commands::SignP2pPtkProposals => steps::sign_p2p_ptk_proposals(&node_config, &dirs).await?,
        Commands::SubmitFinalProposals => {
            steps::submit_final_proposals(&node_config, &dirs).await?
        }
        Commands::PrepareSubmissions => steps::prepare_submissions(&node_config, &dirs).await?,
        Commands::SignSubmissions => steps::sign_submissions(&node_config, &dirs).await?,
        Commands::ExecuteSubmissions => steps::execute_submissions(&node_config, &dirs).await?,
    }

    tracing::info!("Command completed successfully");
    Ok(())
}

/// Start the node as either coordinator or attestor
async fn start_node(node_config: NodeConfig) -> Result {
    // Load network config
    let network_config_path = if PathBuf::from(&node_config.network_config).is_absolute() {
        PathBuf::from(&node_config.network_config)
    } else {
        // Resolve relative to config directory (test-configs/)
        std::env::current_dir()?
            .join("test-configs")
            .join(&node_config.network_config)
    };

    tracing::info!(
        "Loading network config from: {}",
        network_config_path.display()
    );
    let network_config = NetworkConfig::from_file(&network_config_path).await?;

    // Determine if we're the coordinator
    let is_coordinator = match network_config.network.coordinator_strategy {
        CoordinatorStrategy::Election => {
            // Run leader election
            tracing::info!("Running leader election (Bully algorithm)");
            let election_result =
                election::run_election(&network_config, &node_config.node.participant_id).await?;

            tracing::info!(
                "Election complete: {} is the coordinator",
                election_result.coordinator.id
            );

            election_result.is_me
        }
        _ => {
            // Use static coordinator determination (explicit or first)
            network_config.is_coordinator(&node_config.node.participant_id)?
        }
    };

    if is_coordinator {
        tracing::info!("Starting as COORDINATOR");
        start_coordinator(node_config, network_config).await
    } else {
        tracing::info!("Starting as ATTESTOR");
        start_attestor(node_config, network_config).await
    }
}

/// Start node in coordinator mode (server)
async fn start_coordinator(node_config: NodeConfig, network_config: NetworkConfig) -> Result {
    tracing::info!("Initializing Noise server...");

    let server = NoiseServer::new(node_config.clone(), network_config).await?;
    let server = Arc::new(server);

    // Initialize directory paths
    let dirs = WorkflowDirs::new();
    dirs.create_required_dirs().await?;

    tracing::info!("Noise server initialized, listening for connections");

    // Spawn coordinator workflow task
    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let dirs_clone = dirs.clone();
    let workflow_handle = tokio::spawn(async move {
        tracing::info!("Coordinator workflow task started");
        match run_coordinator_workflow(workflow_state, node_config_clone, dirs_clone).await {
            Ok(_) => {
                tracing::info!("Coordinator workflow task completed successfully");
                Ok(())
            }
            Err(e) => {
                tracing::error!("Coordinator workflow task failed: {e}");
                tracing::error!("Error details: {e:?}");
                Err(e)
            }
        }
    });

    // Start server and workflow concurrently
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
                    tracing::error!("Workflow failed, shutting down coordinator");
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

/// Run the coordinator workflow, executing coordinator-only steps
async fn run_coordinator_workflow(
    workflow_state: Arc<WorkflowState>,
    node_config: NodeConfig,
    dirs: WorkflowDirs,
) -> Result {
    use dec_party_onboarding::workflow_state::WorkflowStep;
    use std::collections::HashSet;

    // Track which steps the coordinator has already executed
    let mut coordinator_completed_steps = HashSet::new();

    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            WorkflowStep::WaitingForAttestors => {
                // Wait for all attestors to connect
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            WorkflowStep::UploadDars => {
                // Coordinator must also upload DARs (only once)
                if !coordinator_completed_steps.contains(&WorkflowStep::UploadDars) {
                    tracing::info!("Coordinator executing: Upload DARs");
                    match steps::upload_dars(&node_config, &dirs).await {
                        Ok(_) => {
                            tracing::info!("Coordinator successfully uploaded DARs");
                            coordinator_completed_steps.insert(WorkflowStep::UploadDars);
                        }
                        Err(e) => {
                            tracing::error!("Coordinator failed to upload DARs: {e}");
                            tracing::error!("Error details: {e:?}");
                            return Err(e);
                        }
                    }
                }
                // Now wait for attestors to complete
                tracing::debug!("Coordinator waiting for attestors to complete UploadDars");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            WorkflowStep::GenerateKeys => {
                // Coordinator must also generate keys (only once)
                if !coordinator_completed_steps.contains(&WorkflowStep::GenerateKeys) {
                    tracing::info!("Coordinator executing: Generate keys");
                    match steps::generate_keys(&node_config, &dirs).await {
                        Ok(_) => {
                            tracing::info!("Coordinator successfully generated keys");
                            coordinator_completed_steps.insert(WorkflowStep::GenerateKeys);

                            // Wait for Canton to process namespace delegation proposals
                            tracing::info!(
                                "Waiting 3 seconds for Canton to process namespace delegations..."
                            );
                            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                        }
                        Err(e) => {
                            tracing::error!("Coordinator failed to generate keys: {e}");
                            tracing::error!("Error details: {e:?}");
                            return Err(e);
                        }
                    }
                }
                // Now wait for attestors to complete
                tracing::debug!("Coordinator waiting for attestors to complete GenerateKeys");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            WorkflowStep::CreateProposals => {
                tracing::info!("Coordinator executing: Create proposals");
                if let Err(e) = steps::create_proposals(&node_config, &dirs).await {
                    tracing::error!("Failed to create proposals: {e}");
                    return Err(e);
                }
                workflow_state.advance_step().await;
            }
            WorkflowStep::SignDns => {
                // Attestors are signing, wait for them
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            WorkflowStep::SubmitDns => {
                tracing::info!("Coordinator executing: Submit DNS proposals");

                // Save collected DNS signatures to files
                let attestor_data = workflow_state.get_all_attestor_data().await;
                for (attestor_id, signature_data) in attestor_data {
                    let file_path = dirs
                        .dns_signed_dir
                        .join(format!("signed-dns-proposal-{attestor_id}.bin"));
                    tokio::fs::write(&file_path, signature_data).await?;
                }
                workflow_state.clear_attestor_data().await;

                if let Err(e) = steps::submit_dns_proposals(&node_config, &dirs).await {
                    tracing::error!("Failed to submit DNS proposals: {e}");
                    return Err(e);
                }
                workflow_state.advance_step().await;
            }
            WorkflowStep::SignP2pPtk => {
                // Attestors are signing, wait for them
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            WorkflowStep::SubmitFinal => {
                tracing::info!("Coordinator executing: Submit final proposals");

                // Save collected P2P/PTK signatures to files
                let attestor_data = workflow_state.get_all_attestor_data().await;
                for (attestor_id, signatures_data) in attestor_data {
                    let file_path = dirs
                        .final_signed_dir
                        .join(format!("signed-p2p-ptk-proposals-{attestor_id}.bin"));
                    tokio::fs::write(&file_path, signatures_data).await?;
                }
                workflow_state.clear_attestor_data().await;

                if let Err(e) = steps::submit_final_proposals(&node_config, &dirs).await {
                    tracing::error!("Failed to submit final proposals: {e}");
                    return Err(e);
                }
                workflow_state.advance_step().await;
            }
            WorkflowStep::PrepareSubmissions => {
                tracing::info!("Coordinator executing: Prepare submissions");
                if let Err(e) = steps::prepare_submissions(&node_config, &dirs).await {
                    tracing::error!("Failed to prepare submissions: {e}");
                    return Err(e);
                }
                workflow_state.advance_step().await;
            }
            WorkflowStep::SignSubmissions => {
                // Attestors are signing, wait for them
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            WorkflowStep::ExecuteSubmissions => {
                tracing::info!("Coordinator executing: Execute submissions");

                // Save collected submission signatures to files
                let attestor_data = workflow_state.get_all_attestor_data().await;
                for (attestor_id, signatures_data) in attestor_data {
                    let file_path = dirs
                        .workflow_dir
                        .join(format!("submission-signatures-{attestor_id}.bin"));
                    tokio::fs::write(&file_path, signatures_data).await?;
                }
                workflow_state.clear_attestor_data().await;

                if let Err(e) = steps::execute_submissions(&node_config, &dirs).await {
                    tracing::error!("Failed to execute submissions: {e}");
                    return Err(e);
                }
                workflow_state.advance_step().await;
            }
            WorkflowStep::Complete => {
                tracing::info!("Coordinator workflow complete!");
                tracing::info!("Waiting for attestors to receive disconnect command...");
                // Give attestors time to poll and receive the Disconnect command
                // before we shut down the server
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}

/// Start node in attestor mode (client)
async fn start_attestor(node_config: NodeConfig, network_config: NetworkConfig) -> Result {
    tracing::info!("Initializing Noise client...");

    let client = NoiseClient::new(node_config.clone(), network_config).await?;

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
                    anyhow::bail!("Attestor failed: persistent communication errors with coordinator");
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        tracing::info!("Received command: {command:?}");

        let result = match command {
            MessageType::Wait => {
                // Coordinator says to wait, poll again after delay
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                continue;
            }
            MessageType::Disconnect => {
                tracing::info!("Received disconnect command, shutting down");
                break;
            }
            MessageType::UploadDars => {
                tracing::info!("Executing: Upload DARs");
                match steps::upload_dars(&node_config, &dirs).await {
                    Ok(_) => {
                        // Notify coordinator that we completed this step
                        match client.send_status(b"UploadDars completed".to_vec()).await {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                tracing::error!("Failed to send completion status: {e}");
                                Err(e.into())
                            }
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            MessageType::GenerateKeys => {
                tracing::info!("Executing: Generate keys");
                match steps::generate_keys(&node_config, &dirs).await {
                    Ok(_) => {
                        // Send generated keys to coordinator
                        match send_keys_to_coordinator(&client, &dirs).await {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                tracing::error!("Failed to send keys to coordinator: {e}");
                                Err(e)
                            }
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            MessageType::SignDns => {
                tracing::info!("Executing: Sign DNS proposal");
                match steps::sign_dns_proposals(&node_config, &dirs).await {
                    Ok(_) => {
                        // Send DNS signature to coordinator
                        match send_dns_signature_to_coordinator(&client, &dirs).await {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                tracing::error!("Failed to send DNS signature to coordinator: {e}");
                                Err(e)
                            }
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            MessageType::SignP2pPtk => {
                tracing::info!("Executing: Sign P2P proposals (Canton 3.4+: PTK deprecated)");
                match steps::sign_p2p_ptk_proposals(&node_config, &dirs).await {
                    Ok(_) => {
                        // Send P2P signatures to coordinator
                        match send_p2p_ptk_signatures_to_coordinator(&client, &dirs).await {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                tracing::error!(
                                    "Failed to send P2P signatures to coordinator: {e}"
                                );
                                Err(e)
                            }
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            MessageType::SignSubmissions => {
                tracing::info!("Executing: Sign submissions");
                match steps::sign_submissions(&node_config, &dirs).await {
                    Ok(_) => {
                        // Send submission signatures to coordinator
                        match send_submission_signatures_to_coordinator(&client, &dirs).await {
                            Ok(_) => Ok(()),
                            Err(e) => {
                                tracing::error!(
                                    "Failed to send submission signatures to coordinator: {e}"
                                );
                                Err(e)
                            }
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            _ => {
                tracing::warn!("Unexpected message type: {command:?}");
                continue;
            }
        };

        // Handle step result
        if let Err(e) = result {
            tracing::error!("Step execution failed: {e}");
            // Continue polling despite error - coordinator will handle timeout/retry
        }
    }

    tracing::info!("Attestor shutting down");
    Ok(())
}

/// Send generated keys to coordinator
async fn send_keys_to_coordinator(client: &NoiseClient, dirs: &WorkflowDirs) -> Result {
    // Find the attestor public keys file in the keys directory
    let mut entries = tokio::fs::read_dir(&dirs.keys_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("attestor-public-keys-"))
            .unwrap_or(false)
        {
            let keys_data = tokio::fs::read(&path).await?;
            client.upload_keys(keys_data).await?;
            return Ok(());
        }
    }

    anyhow::bail!(
        "Attestor public keys file not found in {}",
        dirs.keys_dir.display()
    )
}

/// Send DNS signature to coordinator
async fn send_dns_signature_to_coordinator(client: &NoiseClient, dirs: &WorkflowDirs) -> Result {
    // Find the signed DNS proposal file
    let signed_proposals_dir = &dirs.dns_signed_dir;
    let mut entries = tokio::fs::read_dir(signed_proposals_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("signed-dns-proposal"))
            .unwrap_or(false)
        {
            let signature_data = tokio::fs::read(&path).await?;
            client.send_dns_signature(signature_data).await?;
            return Ok(());
        }
    }

    anyhow::bail!("Signed DNS proposal file not found")
}

/// Send P2P signatures to coordinator
/// Canton 3.4+: PTK deprecated, only P2P proposals
async fn send_p2p_ptk_signatures_to_coordinator(
    client: &NoiseClient,
    dirs: &WorkflowDirs,
) -> Result {
    // Find the signed P2P proposals file
    let signed_proposals_dir = &dirs.final_signed_dir;
    let mut entries = tokio::fs::read_dir(signed_proposals_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("signed-p2p-proposals"))
            .unwrap_or(false)
        {
            let signatures_data = tokio::fs::read(&path).await?;
            client.send_p2p_ptk_signatures(signatures_data).await?;
            return Ok(());
        }
    }

    anyhow::bail!("Signed P2P proposals file not found")
}

/// Send submission signatures to coordinator
async fn send_submission_signatures_to_coordinator(
    client: &NoiseClient,
    dirs: &WorkflowDirs,
) -> Result {
    // Find the submission signatures file in the execution/signatures directory
    let signatures_dir = dirs.workflow_dir.join("execution").join("signatures");
    let mut entries = tokio::fs::read_dir(&signatures_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("submission-signatures-") && n.ends_with(".bin"))
            .unwrap_or(false)
        {
            let signatures_data = tokio::fs::read(&path).await?;
            client.send_submission_signatures(signatures_data).await?;
            return Ok(());
        }
    }

    anyhow::bail!(
        "Submission signatures file not found in {}",
        signatures_dir.display()
    )
}
