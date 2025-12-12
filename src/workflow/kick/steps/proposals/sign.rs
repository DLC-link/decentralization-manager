use tokio::fs;

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::SignedTopologyTransaction,
    topology::admin::v30::{
        SignTransactionsRequest, StoreId, Synchronizer, store_id, synchronizer,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use crate::{
    config::NodeConfig,
    consts::{DNS_KICK_PROTO_FILENAME, P2P_KICK_PROTO_FILENAME, SIGNED_KICK_PROPOSALS_PREFIX},
    error::Result,
    utils,
    workflow::kick::KickDirs,
};

/// Sign kick proposals
///
/// Each remaining member (not the kicked member) signs both proposals
pub async fn sign_proposals(config: &NodeConfig, dirs: &KickDirs) -> Result {
    tracing::info!("Signing kick proposals...");

    let participant_num = utils::get_participant_number(config).await?;
    tracing::debug!("Determined participant number: {participant_num}");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Read DNS kick proposal
    let dns_file = dirs.kick_proposals_dir.join(DNS_KICK_PROTO_FILENAME);
    tracing::info!(
        "Reading DNS kick proposal from {path}",
        path = dns_file.display()
    );
    let dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&dns_file).await?;

    // Read P2P kick proposal
    let p2p_file = dirs.kick_proposals_dir.join(P2P_KICK_PROTO_FILENAME);
    tracing::info!(
        "Reading P2P kick proposal from {path}",
        path = p2p_file.display()
    );
    let p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&p2p_file).await?;

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
    let output_file = dirs.kick_signed_dir.join(format!(
        "{SIGNED_KICK_PROPOSALS_PREFIX}-{participant_num}.bin"
    ));
    tracing::info!(
        "Saving signed kick proposals to {path}",
        path = output_file.display()
    );

    utils::write_messages_to_file(&response.transactions, &output_file).await?;

    tracing::info!("Kick proposals signed successfully");
    Ok(())
}
