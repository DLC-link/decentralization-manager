use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::SignedTopologyTransaction,
    topology::admin::v30::{
        SignTransactionsRequest, StoreId, Synchronizer, store_id, synchronizer,
    },
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        add_party::steps::export_state::encode_length_prefixed_message,
        storage::{WorkflowStorage, artifact_kinds},
        topology::sign_transactions_with_topology_retry,
    },
};

/// All-peer step: sign both add-party proposals.
///
/// Every invited peer signs — existing members authorize the owner/threshold
/// change with their namespace keys; the new member's signature covers both
/// its namespace joining the DNS owner set and its participant accepting to
/// host the party (Canton auto-selects the appropriate keys).
///
/// `proposal_data` is the `[dns, p2p]` pair from the coordinator (the config
/// item was already stripped by the peer loop). Persists per-peer
/// `SIGNED_ADD_PARTY_DNS` / `SIGNED_ADD_PARTY_P2P` artefacts, each a single
/// `varint(len)||proto` blob.
pub async fn sign_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    tracing::info!("Signing add-party proposals...");

    let node_id = config.participant_id().to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let items = utils::decode_length_prefixed(proposal_data, 2)?;
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

    let response = sign_transactions_with_topology_retry(config, request, "add-party").await?;

    if response.transactions.len() != 2 {
        anyhow::bail!(
            "Expected 2 signed transactions (DNS and P2P), got {count}",
            count = response.transactions.len()
        );
    }

    storage
        .write_artifact(
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            Some(&node_id),
            &encode_length_prefixed_message(&response.transactions[0]),
        )
        .await?;
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_P2P,
            Some(&node_id),
            &encode_length_prefixed_message(&response.transactions[1]),
        )
        .await?;

    tracing::info!("Add-party proposals signed successfully");
    Ok(())
}
