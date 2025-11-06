use std::path::Path;

use tokio::fs;

use crate::{
    config::Config,
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

/// Sign P2P and PTK proposals with attestor's key
///
/// Corresponds to: 03_SignP2PPTKProposals.sc
///
/// This step must be run by each attestor participant (except the coordinator who created the proposals).
/// Each attestor signs both the P2P and PTK proposals with their namespace key.
///
/// # Arguments
/// * `config` - Configuration with Canton connection details
/// * `in_dir` - Directory containing p2p_proto.bin and ptk_proto.bin (usually ./out/step_3)
/// * `out_dir` - Directory to write signed proposals (usually ./out/step_3a/signed-proposals)
/// * `ids_dir` - Directory containing participant ID files to determine participant number
pub async fn sign_p2p_ptk_proposals(
    config: &Config,
    in_dir: &Path,
    out_dir: &Path,
    ids_dir: &Path,
) -> Result {
    tracing::info!("Signing P2P and PTK proposals...");

    // Step 1: Get participant number
    let participant_num = utils::get_participant_number(config, ids_dir).await?;
    tracing::debug!("Determined participant number: {participant_num}");

    // Step 2: Get synchronizer ID
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Step 3: Read the P2P proposal from disk
    let p2p_file = in_dir.join("p2p_proto.bin");
    tracing::info!("Reading P2P proposal from {}", p2p_file.display());
    let p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&p2p_file).await?;

    // Step 4: Read the PTK proposal from disk
    let ptk_file = in_dir.join("ptk_proto.bin");
    tracing::info!("Reading PTK proposal from {}", ptk_file.display());
    let ptk_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&ptk_file).await?;

    // Step 5: Sign both transactions using Canton's TopologyManagerWriteService
    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(SignTransactionsRequest {
        transactions: vec![p2p_transaction, ptk_transaction],
        signed_by: vec![], // Auto-select appropriate signing keys
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::Id(synchronizer_id)),
            })),
        }),
        force_flags: vec![],
    });

    tracing::debug!("Calling SignTransactions RPC for P2P and PTK...");
    let response = topology_client
        .sign_transactions(request)
        .await?
        .into_inner();

    if response.transactions.len() != 2 {
        anyhow::bail!(
            "Expected 2 signed transactions, got {}",
            response.transactions.len()
        );
    }

    // Step 6: Save both signed transactions to one file
    fs::create_dir_all(out_dir).await?;
    let output_file = out_dir.join(format!("signed-p2p-ptk-proposals-{participant_num}.bin"));
    tracing::info!(
        "Saving signed P2P and PTK proposals to {}",
        output_file.display()
    );

    utils::write_messages_to_file(&response.transactions, &output_file).await?;

    tracing::info!("P2P and PTK proposals signed successfully");
    Ok(())
}
