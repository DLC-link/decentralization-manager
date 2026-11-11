use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    SignedTopologyTransaction, topology_mapping,
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    noise::{MessageType, client::NoiseClient},
    utils,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

use crate::workflow::{add_party, topology_version};

pub(super) fn decode_add_party_config(payload: &[u8]) -> Result<add_party::AddPartyConfig> {
    serde_json::from_slice(payload).context("Failed to deserialize add-party config")
}

/// Whether this node is the member being added by the run.
pub(super) fn is_new_member(node_config: &NodeConfig, config: &add_party::AddPartyConfig) -> bool {
    node_config.participant_id() == &config.new_participant_id
}

/// Reply with the add-party skip status for a command that only the new
/// member executes. Completes this peer for the step on the coordinator.
pub(super) async fn send_skip_status(client: &NoiseClient, step: &str) {
    tracing::info!("{step}: not the new member — replying skip");
    if let Err(e) = client
        .send(
            MessageType::StatusUpdate,
            add_party::peer::SKIP_STATUS.to_vec(),
        )
        .await
    {
        tracing::error!("Failed to send skip status for {step}: {e}");
    }
}

/// Extract the resolved decentralized party id from a SignP2p command payload.
/// The payload is a `varint(len)||SignedTopologyTransaction` blob whose
/// `transaction.mapping` is a `PartyToParticipant` mapping carrying `party`
/// (i.e. `{prefix}::{namespace_fingerprint}`). We pull that out so the
/// peer's identity hook can key its `dec_party_identity` rows.
pub(super) fn extract_party_id_from_p2p_payload(payload: &[u8]) -> Result<CantonId> {
    let signed: SignedTopologyTransaction = utils::read_first_message_from_bytes(payload)?;

    let tx = topology_version::decode_transaction(&signed.transaction)
        .context("Failed to decode the TopologyTransaction in the SignedTopologyTransaction")?;
    let mapping = tx
        .mapping
        .and_then(|m| m.mapping)
        .ok_or_else(|| anyhow::anyhow!("TopologyTransaction has no mapping"))?;
    match mapping {
        topology_mapping::Mapping::PartyToParticipant(p2p) => CantonId::parse(&p2p.party),
        other => anyhow::bail!("Expected PartyToParticipant mapping, got {other:?}"),
    }
}

/// Persist the prepared submissions blob received from the coordinator into
/// `workflow_artifacts` keyed by the same zero-padded ordinals the coordinator
/// used. The byte-for-byte payload of each submission is preserved so the
/// peer's sign step decodes them identically to what the coordinator
/// produced.
pub(super) async fn save_prepared_submissions_from_payload(
    payload: &[u8],
    db: &SqlitePool,
    instance_name: &str,
) -> Result {
    let files = utils::decode_files(payload)?;

    tracing::info!(
        "Received {count} prepared submission artefact(s) from coordinator",
        count = files.len()
    );

    for (ordinal, data) in &files {
        db.write_artifact(
            instance_name,
            artifact_kinds::PREPARED_SUBMISSION,
            Some(ordinal),
            data,
        )
        .await?;
        tracing::debug!("Saved prepared submission ordinal {ordinal}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_party_id_rejects_garbage_payload() {
        // Empty and non-proto payloads must error, not panic.
        assert!(extract_party_id_from_p2p_payload(b"").is_err());
        assert!(extract_party_id_from_p2p_payload(b"not-a-valid-proto-blob").is_err());
    }
}
