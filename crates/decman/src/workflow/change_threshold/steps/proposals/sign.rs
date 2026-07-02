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
        storage::{WorkflowStorage, artifact_kinds},
        topology::sign_transactions_with_topology_retry,
    },
};

/// Sign the change-threshold proposals.
///
/// Every party member signs both the DNS and P2P proposals. `proposal_data`
/// contains both proposals received from the coordinator. Persists the
/// per-peer signed DNS / P2P proposals as `SIGNED_CHANGE_THRESHOLD_DNS` /
/// `SIGNED_CHANGE_THRESHOLD_P2P` (keyed by this node's participant id). Each
/// artefact is `varint(len)||proto`, so concatenating them yields exactly the
/// buffer the peer sends to the coordinator.
pub async fn sign_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    tracing::info!("Signing change-threshold proposals...");

    let node_id = config.participant_id().to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Parse the combined proposal data from the coordinator.
    let items = utils::decode_length_prefixed(proposal_data, 2)?;
    tracing::info!("Using change-threshold proposals from coordinator payload");

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

    tracing::debug!("Calling SignTransactions RPC for change-threshold proposals...");
    let response =
        sign_transactions_with_topology_retry(config, request, "change-threshold").await?;

    if response.transactions.len() != 2 {
        anyhow::bail!(
            "Expected 2 signed transactions (DNS and P2P), got {count}",
            count = response.transactions.len()
        );
    }

    let dns_bytes = utils::encode_length_prefixed_message(&response.transactions[0]);
    let p2p_bytes = utils::encode_length_prefixed_message(&response.transactions[1]);

    storage
        .write_artifact(
            instance_name,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            Some(&node_id),
            &dns_bytes,
        )
        .await?;
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
            Some(&node_id),
            &p2p_bytes,
        )
        .await?;

    tracing::info!("Change-threshold proposals signed successfully");
    Ok(())
}
