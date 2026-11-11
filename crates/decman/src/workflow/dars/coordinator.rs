use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::{
        MAX_CHUNKED_TOTAL_SIZE, NoiseError,
        server::{ActiveWorkflow, NoiseServer},
    },
    server::{WorkflowInstance, peer_status::LastSeen},
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
    let server = Arc::new(server);

    tracing::info!("Noise server initialized, listening for connections");

    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let workflow_handle =
        tokio::spawn(async move { run_workflow(workflow_state, node_config_clone, config).await });

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::Dars(server),
        instance,
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

    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            DarsStep::WaitingForPeers => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            DarsStep::UploadDars => {
                // While peers are downloading, this node ONLY serves chunks
                // (handled by the Noise listener). The coordinator's own DAR
                // upload is deferred to `Complete` so its CPU/IO doesn't
                // contend with — and stall — the chunk transfer to peers on
                // CPU-limited nodes.
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            DarsStep::Complete => {
                // All peers have finished downloading (the step only advances
                // here once every peer completed UploadDars), so there's no
                // chunk-serving load left to compete with. Upload our own DARs
                // now. (Reached once, then we break — re-runs only on a resume,
                // where re-uploading is idempotent.)
                tracing::info!("Coordinator executing: Upload DARs");
                contracts::upload_dars(&node_config, &config.dar_files).await?;
                tracing::info!("DARs upload workflow complete!");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}

/// Encode DAR files from config for transmission to peers
///
/// # Errors
///
/// `NoiseError::PayloadTooLarge` when the bundle exceeds what a peer accepts.
/// The start handler refuses such a set, so this catches a resumed run.
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

    let payload = utils::encode_files(&dar_files);
    if payload.len() > MAX_CHUNKED_TOTAL_SIZE {
        return Err(NoiseError::PayloadTooLarge {
            size: payload.len(),
            limit: MAX_CHUNKED_TOTAL_SIZE,
        }
        .into());
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::contracts::DarFile;

    /// A one-file run whose bundle is `bundle_len` bytes long.
    fn run_with_bundle_of(bundle_len: usize) -> DarsConfig {
        let filename = "big.dar".to_string();
        // The bundle frames one file with a 4-byte count and two 4-byte lengths.
        let data = vec![0u8; bundle_len - 12 - filename.len()];
        DarsConfig {
            dar_files: vec![DarFile {
                filename,
                data: STANDARD.encode(data),
            }],
            instance_name: "dars-distribute-1".to_string(),
            peer_ids: Vec::new(),
        }
    }

    #[test]
    fn an_oversized_bundle_fails_on_the_coordinator() -> Result {
        let Err(error) = encode_dars_payload(&run_with_bundle_of(MAX_CHUNKED_TOTAL_SIZE + 1))
        else {
            anyhow::bail!("a bundle one byte over the limit must fail");
        };
        assert!(
            matches!(
                error.downcast_ref::<NoiseError>(),
                Some(NoiseError::PayloadTooLarge { size, limit })
                    if *size == MAX_CHUNKED_TOTAL_SIZE + 1 && *limit == MAX_CHUNKED_TOTAL_SIZE
            ),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn a_bundle_at_the_limit_is_encoded() -> Result {
        let payload = encode_dars_payload(&run_with_bundle_of(MAX_CHUNKED_TOTAL_SIZE))?;
        assert_eq!(payload.len(), MAX_CHUNKED_TOTAL_SIZE);
        Ok(())
    }
}
