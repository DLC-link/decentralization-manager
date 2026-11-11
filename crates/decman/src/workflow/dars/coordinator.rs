use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};
use tokio::time::Duration;

use super::{DarsConfig, DarsStep};
use crate::{
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{CoordinatorSteps, StepFlow, contracts, run_coordinator, state::WorkflowState},
};

/// Run the DARs workflow on this node as its coordinator: send the DARs to the
/// invited peers, then upload them here.
///
/// # Errors
///
/// Returns an error if the Noise key file cannot be read, or if a DAR's base64
/// data does not decode. Returns an error if the Admin API channel cannot be
/// opened or `UploadDar` fails on this node. Returns an error if the peers the
/// run waits on give up or go silent, or if the workflow task panics.
pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    config: DarsConfig,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
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

    tracing::info!("Noise server initialized, listening for connections");

    let steps = DarsSteps::start(server.get_workflow_state(), node_config.clone(), config);
    run_coordinator(server, ActiveWorkflow::Dars, instance, steps).await
}

/// The DARs coordinator's work at each step.
struct DarsSteps {
    node_config: NodeConfig,
    config: DarsConfig,
}

impl DarsSteps {
    /// Seed the first command with the encoded DARs.
    async fn start(
        workflow_state: Arc<WorkflowState<DarsStep>>,
        node_config: NodeConfig,
        config: DarsConfig,
    ) -> Result<Self> {
        // Encode DAR files to send to peers with UploadDars command
        let dar_payload = encode_dars_payload(&config)?;
        workflow_state.set_command_payload(dar_payload).await;

        Ok(Self {
            node_config,
            config,
        })
    }
}

impl CoordinatorSteps<DarsStep> for DarsSteps {
    async fn run_step(
        &mut self,
        _workflow_state: &WorkflowState<DarsStep>,
        current_step: DarsStep,
    ) -> Result<StepFlow> {
        match current_step {
            DarsStep::WaitingForPeers => Ok(StepFlow::Idle),
            DarsStep::UploadDars => {
                // While peers are downloading, this node ONLY serves chunks
                // (handled by the Noise listener). The coordinator's own DAR
                // upload is deferred to `Complete` so its CPU/IO doesn't
                // contend with — and stall — the chunk transfer to peers on
                // CPU-limited nodes.
                Ok(StepFlow::Idle)
            }
            DarsStep::Complete => {
                // All peers have finished downloading (the step only advances
                // here once every peer completed UploadDars), so there's no
                // chunk-serving load left to compete with. Upload our own DARs
                // now. (Reached once, then we break — re-runs only on a resume,
                // where re-uploading is idempotent.)
                tracing::info!("Coordinator executing: Upload DARs");
                contracts::upload_dars(&self.node_config, &self.config.dar_files).await?;
                tracing::info!("DARs upload workflow complete!");
                Ok(StepFlow::Finish(Duration::from_secs(5)))
            }
        }
    }
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
                "Failed to decode base64 DAR data for {filename}: {e}",
                filename = dar_file.filename
            )
        })?;
        dar_files.push((dar_file.filename.clone(), data));
    }

    // Sort for consistent ordering
    dar_files.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(utils::encode_files(&dar_files))
}
