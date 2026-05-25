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
        storage::{WorkflowStorage, artifact_kinds},
        topology::sign_transactions_with_topology_retry,
    },
};

/// Sign a single topology proposal and persist the result.
///
/// `proposal_data` is the `varint(len)||SignedTopologyTransaction` blob the
/// coordinator sent (matches the original on-disk dns_proto.bin /
/// p2p_proto.bin format). The signed result is persisted as a per-peer
/// artefact under `(instance, kind, self_id)` using the same
/// length-prefixed framing.
async fn sign_proposal(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    output_kind: &str,
    proposal_type: &str,
    proposal_data: &[u8],
) -> Result {
    tracing::info!("Signing {proposal_type} proposal...");

    let node_id = config.participant_id().to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    tracing::info!("Using {proposal_type} proposal from coordinator payload");
    let transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(proposal_data)?;

    let request = SignTransactionsRequest {
        transactions: vec![transaction],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        force_flags: vec![],
    };

    tracing::debug!("Calling SignTransactions RPC for {proposal_type}...");
    let response = sign_transactions_with_topology_retry(config, request, proposal_type).await?;

    if response.transactions.is_empty() {
        anyhow::bail!("No signed transaction returned for {proposal_type}");
    }

    // Persist as a single per-peer artefact. Each transaction is written
    // as `varint(len)||proto`; concatenated, this matches the previous on-disk
    // format produced by `write_messages_to_file`.
    let mut buffer = BytesMut::new();
    for tx in &response.transactions {
        let encoded = tx.encode_to_vec();
        prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
        buffer.put_slice(&encoded);
    }

    storage
        .write_artifact(instance_name, output_kind, Some(&node_id), &buffer)
        .await?;

    tracing::info!("{proposal_type} proposal signed successfully");
    Ok(())
}

/// Sign DNS proposal with peer's key
///
/// This step must be run by each peer participant (except the coordinator who created the proposal).
/// Each peer signs the DNS proposal with their namespace key.
pub async fn sign_dns_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    sign_proposal(
        config,
        storage,
        instance_name,
        artifact_kinds::SIGNED_DNS_PROPOSAL,
        "DNS",
        proposal_data,
    )
    .await
}

/// Sign P2P proposals with peer's key
///
/// **Canton 3.4+**: Signing keys are now embedded in the P2P mapping.
/// This function signs the P2P proposal which contains both participant and key information.
pub async fn sign_p2p_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    sign_proposal(
        config,
        storage,
        instance_name,
        artifact_kinds::SIGNED_P2P_PROPOSAL,
        "P2P",
        proposal_data,
    )
    .await
}
