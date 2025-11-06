use std::path::Path;

use tokio::fs;

use crate::{
    config::Config,
    error::Result,
    proto::com::digitalasset::canton::topology::admin::v30::{
        SignTransactionsRequest, StoreId, Synchronizer, store_id, synchronizer,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
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
/// * `in_dir` - Directory containing the dns_proto.bin file (usually ./out/step_2)
/// * `out_dir` - Directory to write signed proposal (usually ./out/step_2a/signed-proposals)
/// * `ids_dir` - Directory containing participant ID files to determine participant number
pub async fn sign_dns_proposals(
    config: &Config,
    in_dir: &Path,
    out_dir: &Path,
    ids_dir: &Path,
) -> Result {
    tracing::info!("Signing DNS proposal...");

    // Step 1: Get current participant ID and find its number in the ids directory
    let participant_id = utils::get_participant_id(config).await?;
    // Add "PAR::" prefix to match the format stored in files (see step_1.rs export_participant_id)
    let participant_id_with_prefix = format!("PAR::{participant_id}");
    let participant_num = find_participant_number(ids_dir, &participant_id_with_prefix).await?;
    tracing::debug!(
        "Current participant ID: {participant_id}, determined number: {participant_num}"
    );

    // Step 2: Get synchronizer ID
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Step 2: Read the DNS proposal from disk
    let dns_file = in_dir.join("dns_proto.bin");
    tracing::info!("Reading DNS proposal from {}", dns_file.display());

    let dns_transaction: crate::proto::com::digitalasset::canton::protocol::v30::SignedTopologyTransaction =
        utils::read_first_message_from_file(&dns_file).await?;

    // Step 3: Sign the transaction using Canton's TopologyManagerWriteService
    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(SignTransactionsRequest {
        transactions: vec![dns_transaction],
        signed_by: vec![], // Auto-select appropriate signing keys
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::Id(synchronizer_id)),
            })),
        }),
        force_flags: vec![],
    });

    tracing::debug!("Calling SignTransactions RPC...");
    let response = topology_client.sign_transactions(request).await?.into_inner();

    let signed_transaction = response
        .transactions
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No signed transaction returned"))?;

    // Step 4: Save the signed transaction to disk
    fs::create_dir_all(out_dir).await?;
    let output_file = out_dir.join(format!("signed-dns-proposal-{participant_num}.bin"));
    tracing::info!("Saving signed DNS proposal to {}", output_file.display());

    utils::write_message_to_file(&signed_transaction, &output_file).await?;

    tracing::info!("DNS proposal signed successfully");
    Ok(())
}

/// Find which participant number corresponds to the current participant ID
///
/// Reads all participant-id-*.bin files and matches the current participant ID
/// against them to determine which number this participant is.
async fn find_participant_number(ids_dir: &Path, current_id: &str) -> Result<u32> {
    let mut dir_entries = fs::read_dir(ids_dir).await?;
    let mut id_files = Vec::new();

    while let Some(entry) = dir_entries.next_entry().await? {
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();
        if file_name_str.starts_with("participant-id") && file_name_str.ends_with(".bin") {
            id_files.push(entry.path());
        }
    }

    if id_files.is_empty() {
        anyhow::bail!("No participant ID files found in {}", ids_dir.display());
    }

    id_files.sort();

    // Read each file and match against current participant ID
    for (idx, id_file) in id_files.iter().enumerate() {
        let id_bytes = fs::read(id_file).await?;
        let stored_id = String::from_utf8(id_bytes)?;

        if stored_id == current_id {
            return Ok((idx + 1) as u32);
        }
    }

    anyhow::bail!(
        "Current participant ID '{}' not found in ids directory",
        current_id
    )
}
