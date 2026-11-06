use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    noise::client::NoiseClient,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

/// Send the locally-produced submission signatures bundle to the coordinator.
///
/// The artefact (`SUBMISSION_SIGNATURES`) was written by `sign_submissions`
/// as a multi-message `varint(len)||proto` blob keyed by this node's
/// participant id. The bytes shipped to the coordinator are byte-identical to
/// what the previous file-based implementation read from
/// `submission-signatures-{node_id}.bin`, so the coordinator's `execute_submissions`
/// step decodes them unchanged.
pub async fn send_submission_signatures_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let node_id = node_config.participant_id().to_string();

    let data = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SUBMISSION_SIGNATURES,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "SUBMISSION_SIGNATURES artifact missing for {node_id} on {instance_name}"
            )
        })?;

    client.send_submission_signatures(data).await?;
    Ok(())
}
