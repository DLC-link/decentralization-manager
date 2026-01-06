use tokio::fs;

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::SignedTopologyTransaction,
    topology::admin::v30::{
        SignTransactionsRequest, StoreId, Synchronizer, store_id, synchronizer,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use crate::{
    config::NodeConfig, consts::SIGNED_KICK_PROPOSALS_PREFIX, error::Result, utils,
    workflow::kick::KickDirs,
};

/// Sign kick proposals
///
/// Each remaining member (not the kicked member) signs both proposals.
/// `proposal_data` contains both DNS and P2P proposals received from coordinator.
pub async fn sign_proposals(config: &NodeConfig, dirs: &KickDirs, proposal_data: &[u8]) -> Result {
    tracing::info!("Signing kick proposals...");

    let node_id = config.node.participant_id.to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Parse the combined proposal data from coordinator
    let items = utils::decode_length_prefixed(proposal_data, 2)?;
    tracing::info!("Using kick proposals from coordinator payload");

    let dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&items[0])?;
    let p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&items[1])?;

    // Sign both proposals
    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(SignTransactionsRequest {
        transactions: vec![dns_transaction, p2p_transaction],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        force_flags: vec![],
    });

    tracing::debug!("Calling SignTransactions RPC for kick proposals...");
    let response = topology_client
        .sign_transactions(request)
        .await?
        .into_inner();

    if response.transactions.len() != 2 {
        anyhow::bail!(
            "Expected 2 signed transactions (DNS and P2P), got {count}",
            count = response.transactions.len()
        );
    }

    // Save signed proposals
    fs::create_dir_all(&dirs.kick_signed_dir).await?;
    let output_file = dirs
        .kick_signed_dir
        .join(format!("{SIGNED_KICK_PROPOSALS_PREFIX}-{node_id}.bin"));
    tracing::info!(
        "Saving signed kick proposals to {path}",
        path = output_file.display()
    );

    utils::write_messages_to_file(&response.transactions, &output_file).await?;

    tracing::info!("Kick proposals signed successfully");
    Ok(())
}
