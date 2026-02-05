use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use tokio::fs;

use crate::{
    auth::WorkflowAuth,
    config::{NetworkConfig, NodeConfig},
    consts::{
        EXECUTION_DIR, LEDGER_SUBMISSIONS_DIR, PREPARED_DIR, PREPARED_SUBMISSION_PREFIX,
        SIGNATURES_DIR, SUBMISSION_SIGNATURES_PREFIX,
    },
    error::Result,
    noise::server::NoiseServer,
    utils,
    workflow::state::WorkflowState,
};

use super::{
    ContractsConfig, ContractsDirs, ContractsStep,
    steps::{execute_submissions, prepare_submissions, sign_submissions, upload_dars},
};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    config: ContractsConfig,
    workflow_auth: Option<WorkflowAuth>,
) -> Result {
    tracing::info!("Initializing Noise server...");

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        ContractsStep::WaitingForAttestors,
        None, // No excluded participants
    )
    .await?;
    let server = Arc::new(server);

    let dirs = ContractsDirs::with_base(
        node_config.workflow_data_dir(),
        &config.instance_name,
        &config.decentralized_party_id.prefix,
        node_config.dars_dir(),
    );
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
            config,
            workflow_auth,
        )
        .await
    });

    crate::workflow::run_server_with_workflow(server, workflow_handle).await
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<ContractsStep>>,
    node_config: NodeConfig,
    network_config: NetworkConfig,
    dirs: ContractsDirs,
    config: ContractsConfig,
    workflow_auth: Option<WorkflowAuth>,
) -> Result {
    // Get credentials for the decentralized party
    let dec_party_id = &config.decentralized_party_id;
    let auth = workflow_auth
        .ok_or_else(|| anyhow::anyhow!("Auth not configured, cannot run contracts workflow"))?;
    let creds = auth.get_credentials(dec_party_id).await?;
    let token = creds.token;
    let user_id = creds.user_id;

    let mut coordinator_completed_steps = HashSet::new();

    // Encode DAR files to send to attestors with UploadDars command
    let dar_payload = encode_dars_payload(&config)?;
    workflow_state.set_command_payload(dar_payload).await;

    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            ContractsStep::WaitingForAttestors => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::UploadDars => {
                if !coordinator_completed_steps.contains(&ContractsStep::UploadDars) {
                    tracing::info!("Coordinator executing: Upload DARs");
                    upload_dars(&node_config, &config).await?;
                    coordinator_completed_steps.insert(ContractsStep::UploadDars);
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::PrepareSubmissions => {
                tracing::info!("Coordinator executing: Prepare submissions");
                prepare_submissions(
                    &node_config,
                    &dirs,
                    &network_config,
                    &config,
                    &token,
                    &user_id,
                )
                .await?;

                // Load prepared submissions to send to attestors with SignSubmissions command
                // Prepend config so attestors know the instance_name for directory creation
                let submissions_payload = load_prepared_submissions_payload(&dirs, &config).await?;
                workflow_state
                    .set_command_payload(submissions_payload)
                    .await;

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
                let signatures_dir = dirs.workflow_dir.join(EXECUTION_DIR).join(SIGNATURES_DIR);
                utils::create_directory(&signatures_dir).await?;
                crate::workflow::save_attestor_data(
                    &workflow_state,
                    &signatures_dir,
                    SUBMISSION_SIGNATURES_PREFIX,
                )
                .await?;
                execute_submissions(&node_config, &dirs, &config, &token, &user_id).await?;
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

/// Encode DAR files from config for transmission to attestors
fn encode_dars_payload(config: &ContractsConfig) -> Result<Vec<u8>> {
    if config.dar_files.is_empty() {
        tracing::debug!("No DAR files to distribute to attestors");
        return Ok(Vec::new());
    }

    tracing::debug!(
        "Encoding {count} DAR file(s) for distribution to attestors",
        count = config.dar_files.len()
    );

    let mut dar_files = Vec::new();

    for dar_file in &config.dar_files {
        let data = STANDARD.decode(&dar_file.data).map_err(|e| {
            anyhow::anyhow!(
                "Failed to decode base64 DAR data for {}: {e}",
                dar_file.filename
            )
        })?;
        dar_files.push((dar_file.filename.clone(), data));
    }

    // Sort for consistent ordering
    dar_files.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(utils::encode_files(&dar_files))
}

/// Load all prepared submission files and encode them for transmission
/// Returns: [config_json_len][config_json][files_payload]
async fn load_prepared_submissions_payload(
    dirs: &ContractsDirs,
    config: &ContractsConfig,
) -> Result<Vec<u8>> {
    let prepared_dir = dirs
        .workflow_dir
        .join(LEDGER_SUBMISSIONS_DIR)
        .join(PREPARED_DIR);

    tracing::debug!(
        "Loading prepared submissions from {path} for distribution",
        path = prepared_dir.display()
    );

    let submission_files =
        utils::find_files_by_pattern(&prepared_dir, PREPARED_SUBMISSION_PREFIX, ".bin").await?;

    if submission_files.is_empty() {
        anyhow::bail!(
            "No prepared submission files found in {path}",
            path = prepared_dir.display()
        );
    }

    let mut files = Vec::new();
    for path in submission_files {
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown.bin")
            .to_string();
        let data = fs::read(&path).await?;
        files.push((filename, data));
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));

    tracing::debug!(
        "Loaded {count} prepared submission file(s) for distribution to attestors",
        count = files.len()
    );

    // Encode config + files payload
    let config_data = serde_json::to_vec(config).context("Failed to serialize contracts config")?;
    let files_payload = utils::encode_files(&files);
    Ok(utils::encode_length_prefixed(&[
        &config_data,
        &files_payload,
    ]))
}
