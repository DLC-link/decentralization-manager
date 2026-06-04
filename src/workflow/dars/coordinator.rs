use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{ActiveWorkflowSlot, peer_status::LastSeen},
    utils,
    workflow::contracts,
};

use super::{DarsConfig, DarsStep};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    config: DarsConfig,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    active_workflow: ActiveWorkflowSlot,
) -> Result {
    tracing::info!("Initializing Noise server for DARs upload...");

    let self_id = node_config.participant_id();
    let excluded: Vec<String> = network_config
        .peers
        .iter()
        .filter(|p| &p.participant_id != self_id && !config.peer_ids.contains(&p.participant_id))
        .map(|p| p.participant_id.to_string())
        .collect();

    let server = NoiseServer::new(
        node_config.clone(),
        network_config,
        db,
        config.instance_name.clone(),
        DarsStep::WaitingForPeers,
        Some(excluded),
        last_seen,
    )
    .await?;
    let server = Arc::new(server);

    tracing::info!("Noise server initialized, listening for connections");

    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let workflow_handle =
        tokio::spawn(async move { run_workflow(workflow_state, node_config_clone, config).await });

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::Dars(server),
        active_workflow,
        workflow_handle,
    )
    .await
}

async fn run_workflow(
    workflow_state: Arc<crate::workflow::state::WorkflowState<DarsStep>>,
    node_config: NodeConfig,
    config: DarsConfig,
) -> Result {
    // Encode DAR files to send to peers with UploadDars command
    let dar_payload = encode_dars_payload(&config)?;
    workflow_state.set_command_payload(dar_payload).await;

    let mut coordinator_completed = false;

    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            DarsStep::WaitingForPeers => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            DarsStep::UploadDars => {
                if !coordinator_completed {
                    tracing::info!("Coordinator executing: Upload DARs");
                    contracts::upload_dars(&node_config, &config.dar_files).await?;
                    coordinator_completed = true;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            DarsStep::Complete => {
                tracing::info!("DARs upload workflow complete!");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}

/// Encode DAR files from config for transmission to peers
fn encode_dars_payload(config: &DarsConfig) -> Result<Vec<u8>> {
    if config.dar_files.is_empty() {
        tracing::info!("No DAR files to distribute to peers");
        return Ok(Vec::new());
    }

    tracing::info!(
        "Encoding {count} DAR file(s) for distribution to peers",
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
