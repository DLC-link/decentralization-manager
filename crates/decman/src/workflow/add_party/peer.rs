use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    noise::client::NoiseClient,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

/// Status string a non-addressed peer replies with when a new-member-only
/// command (GenerateAddPartyKeys / ImportAcs / ClearOnboardingFlag) isn't for
/// it. Any status completes the peer for the step — the constant just keeps
/// the coordinator logs readable.
pub const SKIP_STATUS: &[u8] = b"skipped (not the new member)";

/// Send the new member's `keys||participant_id` blob to the coordinator —
/// same two-item length-prefixed payload onboarding peers send, so the
/// coordinator's split-and-save path is shared.
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

    let combined_payload = crate::utils::encode_length_prefixed(&[&keys_data, &id_data]);
    client.upload_add_party_keys(combined_payload).await?;
    Ok(())
}

/// Send this peer's signed DNS + P2P add-party proposals to the coordinator
/// as one concatenated buffer (two `varint(len)||proto` blobs back to back),
/// mirroring the kick signature wire format.
pub async fn send_add_party_signatures_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let node_id = node_config.participant_id().to_string();

    let dns = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_ADD_PARTY_DNS artifact missing for {node_id}"))?;
    let p2p = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_P2P,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_ADD_PARTY_P2P artifact missing for {node_id}"))?;

    let mut payload = Vec::with_capacity(dns.len() + p2p.len());
    payload.extend_from_slice(&dns);
    payload.extend_from_slice(&p2p);

    client.send_add_party_signatures(payload).await?;
    Ok(())
}

/// Send this peer's signed onboarding-flag clearing proposal (a single
/// `varint(len)||proto` blob) to the coordinator.
pub async fn send_clear_signature_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let node_id = node_config.participant_id().to_string();

    let data = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_ADD_PARTY_CLEAR artifact missing for {node_id}"))?;

    client.send_add_party_clear_signature(data).await?;
    Ok(())
}
