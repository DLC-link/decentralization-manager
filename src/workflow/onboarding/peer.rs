use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    noise::client::NoiseClient,
    utils,
    workflow::storage::{WorkflowStorage, artifact_kinds, identity_kinds},
};

/// Send the peer's own (namespace_key, daml_key) blob + participant id to
/// the coordinator. Both artefacts were just persisted by `generate_keys` to
/// `workflow_artifacts` keyed by this peer's canton id; we read them back
/// and combine into a single length-prefixed payload (decoded as 2 items by
/// the coordinator).
pub async fn send_keys_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let self_id = node_config.participant_id().to_string();

    let keys_data = storage
        .read_artifact(
            instance_name,
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&self_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("PEER_PUBLIC_KEYS artifact missing for {self_id}"))?;

    let id_data = storage
        .read_artifact(
            instance_name,
            artifact_kinds::PARTICIPANT_ID,
            Some(&self_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("PARTICIPANT_ID artifact missing for {self_id}"))?;

    let combined_payload = utils::encode_length_prefixed(&[&keys_data, &id_data]);

    tracing::debug!(
        "Sending combined payload: {keys_len} bytes keys + {id_len} bytes participant ID",
        keys_len = keys_data.len(),
        id_len = id_data.len()
    );

    client.upload_keys(combined_payload).await?;
    Ok(())
}

/// Send this peer's signed DNS proposal (the `SIGNED_DNS_PROPOSAL`
/// artefact written by `sign_dns_proposals`) to the coordinator.
pub async fn send_dns_signature_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let self_id = node_config.participant_id().to_string();

    let data = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_DNS_PROPOSAL,
            Some(&self_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_DNS_PROPOSAL artifact missing for {self_id}"))?;

    client.send_dns_signature(data).await?;
    Ok(())
}

/// Send this peer's signed P2P proposal (the `SIGNED_P2P_PROPOSAL`
/// artefact written by `sign_p2p_proposals`) to the coordinator.
pub async fn send_p2p_signatures_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let self_id = node_config.participant_id().to_string();

    let data = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_P2P_PROPOSAL,
            Some(&self_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_P2P_PROPOSAL artifact missing for {self_id}"))?;

    client.send_p2p_signatures(data).await?;
    Ok(())
}

/// Identity hook (peer side): once the dec_party_id is known (extracted
/// from the P2P proposal's `party` field at SignP2p time), copy this
/// peer's own `PEER_PUBLIC_KEYS` + `PARTICIPANT_ID` artefacts into
/// `dec_party_identity` keyed by `(party_id, self_id)`. These rows survive
/// the workflow_runs row's eventual dismissal and are read by post-onboarding
/// workflows on this node (e.g. contracts::sign_submissions).
pub async fn copy_self_identity_for_party(
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
    dec_party_id: &CantonId,
) -> Result {
    let self_id = node_config.participant_id().to_string();

    if let Some(keys) = storage
        .read_artifact(
            instance_name,
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&self_id),
        )
        .await?
    {
        storage
            .write_identity(
                dec_party_id,
                identity_kinds::PEER_PUBLIC_KEYS,
                &self_id,
                &keys,
            )
            .await?;
    }

    if let Some(pid) = storage
        .read_artifact(
            instance_name,
            artifact_kinds::PARTICIPANT_ID,
            Some(&self_id),
        )
        .await?
    {
        storage
            .write_identity(dec_party_id, identity_kinds::PARTICIPANT_ID, &self_id, &pid)
            .await?;
    }

    Ok(())
}
