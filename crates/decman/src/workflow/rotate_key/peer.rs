use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    noise::client::NoiseClient,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

/// Send this node's signed rotate-key proposal to the coordinator.
pub async fn send_rotate_key_signatures_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let node_id = node_config.participant_id().to_string();
    let signed = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_ROTATE_KEY_P2P,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_ROTATE_KEY_P2P artifact missing for {node_id}"))?;
    client.send_rotate_key_signatures(signed).await?;
    Ok(())
}
