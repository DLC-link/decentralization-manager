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
    consts::{DNS_PROTO_FILENAME, SIGNED_DNS_PROPOSAL_PREFIX},
    dirs::WorkflowDirs,
    error::Result,
    utils,
};

/// Sign DNS proposal with attestor's key
///
/// Corresponds to: 02_SignProposals.sc
///
/// This step must be run by each attestor participant (except the coordinator who created the proposal).
/// Each attestor signs the DNS proposal with their namespace key.
///
/// # Arguments
/// * `config` - Configuration with Canton connection details
/// * `dirs` - WorkflowDirs containing all directory paths
pub async fn sign_dns_proposals(config: &NodeConfig, dirs: &WorkflowDirs) -> Result {
    tracing::info!("Signing DNS proposal...");

    // Step 1: Get participant number
    let participant_num = utils::get_participant_number(config, &dirs.ids_dir).await?;
    tracing::debug!("Determined participant number: {participant_num}");

    // Step 2: Get synchronizer ID
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Step 3: Read the DNS proposal from disk
    let dns_file = dirs.dns_proposals_dir.join(DNS_PROTO_FILENAME);
    tracing::info!("Reading DNS proposal from {}", dns_file.display());

    let dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&dns_file).await?;

    // Step 4: Sign the transaction using Canton's TopologyManagerWriteService
    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(SignTransactionsRequest {
        transactions: vec![dns_transaction],
        signed_by: vec![], // Auto-select appropriate signing keys
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

    // Step 5: Save the signed transaction to disk
    fs::create_dir_all(&dirs.dns_signed_dir).await?;
    let output_file = dirs.dns_signed_dir.join(format!(
        "{SIGNED_DNS_PROPOSAL_PREFIX}-{participant_num}.bin"
    ));
    tracing::info!("Saving signed DNS proposal to {}", output_file.display());

    utils::write_message_to_file(&signed_transaction, &output_file).await?;

    tracing::info!("DNS proposal signed successfully");
    Ok(())
}
