use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    noise::client::NoiseClient,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

/// Send the locally-signed DNS + P2P kick proposals to the coordinator.
///
/// The two artefacts (`SIGNED_KICK_DNS` and `SIGNED_KICK_P2P`) were written
/// by `sign_proposals` as `varint(len)||proto` blobs. Concatenating them
/// produces exactly the buffer the previous file-based implementation
/// shipped (i.e. the contents of the combined signed proposals file), which
/// is what the coordinator's submit step expects to decode as two protobuf
/// messages.
pub async fn send_kick_signatures_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let node_id = node_config.participant_id().to_string();

    let dns = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_KICK_DNS,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_KICK_DNS artifact missing for {node_id}"))?;
    let p2p = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_KICK_P2P,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_KICK_P2P artifact missing for {node_id}"))?;

    let mut payload = Vec::with_capacity(dns.len() + p2p.len());
    payload.extend_from_slice(&dns);
    payload.extend_from_slice(&p2p);

    client.send_kick_signatures(payload).await?;
    Ok(())
}
