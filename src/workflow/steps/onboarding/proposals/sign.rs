use tokio::fs;

use crate::{
    config::NodeConfig,
    consts::{
        DNS_PROTO_FILENAME, P2P_PROTO_FILENAME, SIGNED_DNS_PROPOSAL_PREFIX,
        SIGNED_P2P_PROPOSALS_PREFIX,
    },
    dirs::WorkflowDirs,
    error::Result,
    proto::com::digitalasset::canton::{
        protocol::v30::SignedTopologyTransaction,
        topology::admin::v30::{
            SignTransactionsRequest, StoreId, Synchronizer, store_id, synchronizer,
            topology_manager_write_service_client::TopologyManagerWriteServiceClient,
        },
    },
    utils,
};

/// Sign DNS proposal with attestor's key
///
/// Corresponds to: 02_SignProposals.sc
///
/// This step must be run by each attestor participant (except the coordinator who created the proposal).
/// Each attestor signs the DNS proposal with their namespace key.
pub async fn sign_dns_proposals(config: &NodeConfig, dirs: &WorkflowDirs) -> Result {
    tracing::info!("Signing DNS proposal...");

    let participant_num = utils::get_participant_number(config, &dirs.ids_dir).await?;
    tracing::debug!("Determined participant number: {participant_num}");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let dns_file = dirs.dns_proposals_dir.join(DNS_PROTO_FILENAME);
    tracing::info!("Reading DNS proposal from {}", dns_file.display());

    let dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&dns_file).await?;

    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(SignTransactionsRequest {
        transactions: vec![dns_transaction],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        force_flags: vec![],
    });

    tracing::debug!("Calling SignTransactions RPC...");
    let response = topology_client
        .sign_transactions(request)
        .await?
        .into_inner();

    let signed_transaction = response
        .transactions
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No signed transaction returned"))?;

    fs::create_dir_all(&dirs.dns_signed_dir).await?;
    let output_file = dirs.dns_signed_dir.join(format!(
        "{SIGNED_DNS_PROPOSAL_PREFIX}-{participant_num}.bin"
    ));
    tracing::info!("Saving signed DNS proposal to {}", output_file.display());

    utils::write_message_to_file(&signed_transaction, &output_file).await?;

    tracing::info!("DNS proposal signed successfully");
    Ok(())
}

/// Sign P2P proposals with attestor's key
///
/// Corresponds to: 03_SignP2PProposals.sc
///
/// **Canton 3.4+**: Signing keys are now embedded in the P2P mapping.
/// This function signs the P2P proposal which contains both participant and key information.
///
/// This step must be run by each attestor participant (except the coordinator who created the proposals).
/// Each attestor signs the P2P proposal with their namespace key.
pub async fn sign_p2p_proposals(config: &NodeConfig, dirs: &WorkflowDirs) -> Result {
    tracing::info!("Signing P2P proposals...");

    let participant_num = utils::get_participant_number(config, &dirs.ids_dir).await?;
    tracing::debug!("Determined participant number: {participant_num}");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let p2p_file = dirs.p2p_proposals_dir.join(P2P_PROTO_FILENAME);
    tracing::info!("Reading P2P proposal from {}", p2p_file.display());
    let p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&p2p_file).await?;

    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(SignTransactionsRequest {
        transactions: vec![p2p_transaction],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        force_flags: vec![],
    });

    tracing::debug!("Calling SignTransactions RPC for P2P...");
    let response = topology_client
        .sign_transactions(request)
        .await?
        .into_inner();

    if response.transactions.len() != 1 {
        anyhow::bail!(
            "Expected 1 signed transaction, got {}",
            response.transactions.len()
        );
    }

    fs::create_dir_all(&dirs.final_signed_dir).await?;
    let output_file = dirs.final_signed_dir.join(format!(
        "{SIGNED_P2P_PROPOSALS_PREFIX}-{participant_num}.bin"
    ));
    tracing::info!("Saving signed P2P proposal to {}", output_file.display());

    utils::write_messages_to_file(&response.transactions, &output_file).await?;

    tracing::info!("P2P proposal signed successfully");
    Ok(())
}
