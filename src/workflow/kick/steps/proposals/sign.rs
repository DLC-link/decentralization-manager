use bytes::{BufMut, BytesMut};
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::SignedTopologyTransaction,
    topology::admin::v30::{
        SignTransactionsRequest, StoreId, Synchronizer, store_id, synchronizer,
    },
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        onboarding::steps::proposals::sign::sign_transactions_with_topology_retry,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Sign kick proposals
///
/// Each remaining member (not the kicked member) signs both proposals.
/// `proposal_data` contains both DNS and P2P proposals received from coordinator.
///
/// Persists the per-peer signed DNS / P2P proposals as
/// `SIGNED_KICK_DNS` / `SIGNED_KICK_P2P` artefacts (keyed by this node's
/// participant id). The byte-shape per artefact is `varint(len)||proto`,
/// matching the original on-disk format produced by `write_message_to_file`.
pub async fn sign_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    tracing::info!("Signing kick proposals...");

    let node_id = config.participant_id().to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Parse the combined proposal data from coordinator
    let items = utils::decode_length_prefixed(proposal_data, 2)?;
    tracing::info!("Using kick proposals from coordinator payload");

    let dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&items[0])?;
    let p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&items[1])?;

    let request = SignTransactionsRequest {
        transactions: vec![dns_transaction, p2p_transaction],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        force_flags: vec![],
    };

    tracing::debug!("Calling SignTransactions RPC for kick proposals...");
    let response = sign_transactions_with_topology_retry(config, request, "kick").await?;

    if response.transactions.len() != 2 {
        anyhow::bail!(
            "Expected 2 signed transactions (DNS and P2P), got {count}",
            count = response.transactions.len()
        );
    }

    // Persist signed DNS + P2P as separate per-peer artefacts. Each is
    // written as `varint(len)||proto` so the bytes that go on the wire to the
    // coordinator (which is the concatenation of these two artefacts) are
    // byte-identical to what `write_messages_to_file(&[dns, p2p], path)`
    // produced before — Canton sees the exact same protobufs.
    let dns_bytes = encode_length_prefixed_message(&response.transactions[0]);
    let p2p_bytes = encode_length_prefixed_message(&response.transactions[1]);

    storage
        .write_artifact(
            instance_name,
            artifact_kinds::SIGNED_KICK_DNS,
            Some(&node_id),
            &dns_bytes,
        )
        .await?;
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::SIGNED_KICK_P2P,
            Some(&node_id),
            &p2p_bytes,
        )
        .await?;

    tracing::info!("Kick proposals signed successfully");
    Ok(())
}

/// Encode a protobuf message as `varint(len)||proto`. Matches the on-disk
/// format `utils::write_message_to_file` used to produce; concatenating the
/// DNS and P2P encodings yields exactly the buffer the peer sends to the
/// coordinator over Noise (i.e. the original combined-file bytes).
fn encode_length_prefixed_message<M: Message>(message: &M) -> Vec<u8> {
    let encoded = message.encode_to_vec();
    let mut buffer = BytesMut::new();
    prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
    buffer.put_slice(&encoded);
    buffer.to_vec()
}
