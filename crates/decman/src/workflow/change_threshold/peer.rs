use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    noise::client::NoiseClient,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

/// Send the locally-signed DNS + P2P change-threshold proposals to the
/// coordinator.
///
/// The two artefacts (`SIGNED_CHANGE_THRESHOLD_DNS` and
/// `SIGNED_CHANGE_THRESHOLD_P2P`) were written by `sign_proposals` as
/// `varint(len)||proto` blobs. Concatenating them produces exactly the buffer
/// the coordinator's submit step decodes as two protobuf messages.
pub async fn send_change_threshold_signatures_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let node_id = node_config.participant_id().to_string();

    let dns = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("SIGNED_CHANGE_THRESHOLD_DNS artifact missing for {node_id}")
        })?;
    let p2p = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("SIGNED_CHANGE_THRESHOLD_P2P artifact missing for {node_id}")
        })?;

    let mut payload = Vec::with_capacity(dns.len() + p2p.len());
    payload.extend_from_slice(&dns);
    payload.extend_from_slice(&p2p);

    client.send_change_threshold_signatures(payload).await?;
    Ok(())
}
