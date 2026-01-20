use std::path::Path;

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
    consts::{
        DNS_PROTO_FILENAME, P2P_PROTO_FILENAME, SIGNED_DNS_PROPOSAL_PREFIX,
        SIGNED_P2P_PROPOSALS_PREFIX,
    },
    error::Result,
    utils,
    workflow::onboarding::OnboardingDirs,
};

/// Sign a topology proposal and save the signed transaction
///
/// If `proposal_data` is provided, it's used directly. Otherwise reads from `input_file`.
async fn sign_proposal(
    config: &NodeConfig,
    input_file: &Path,
    output_dir: &Path,
    output_prefix: &str,
    proposal_type: &str,
    proposal_data: Option<&[u8]>,
) -> Result {
    tracing::info!("Signing {proposal_type} proposal...");

    let node_id = config.participant_id().to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Use provided proposal data or read from file
    let transaction: SignedTopologyTransaction = if let Some(data) = proposal_data {
        tracing::info!("Using {proposal_type} proposal from coordinator payload");
        utils::read_first_message_from_bytes(data)?
    } else {
        tracing::info!(
            "Reading {proposal_type} proposal from {path}",
            path = input_file.display()
        );
        utils::read_first_message_from_file(input_file).await?
    };

    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(SignTransactionsRequest {
        transactions: vec![transaction],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        force_flags: vec![],
    });

    tracing::debug!("Calling SignTransactions RPC for {proposal_type}...");
    let response = topology_client
        .sign_transactions(request)
        .await?
        .into_inner();

    if response.transactions.is_empty() {
        anyhow::bail!("No signed transaction returned for {proposal_type}");
    }

    fs::create_dir_all(output_dir).await?;
    let output_file = output_dir.join(format!("{output_prefix}-{node_id}.bin"));
    tracing::info!(
        "Saving signed {proposal_type} proposal to {path}",
        path = output_file.display()
    );

    utils::write_messages_to_file(&response.transactions, &output_file).await?;

    tracing::info!("{proposal_type} proposal signed successfully");
    Ok(())
}

/// Sign DNS proposal with attestor's key
///
/// This step must be run by each attestor participant (except the coordinator who created the proposal).
/// Each attestor signs the DNS proposal with their namespace key.
///
/// # Arguments
/// * `config` - Node configuration with Canton connection details
/// * `dirs` - Directory paths for the onboarding workflow
/// * `proposal_data` - Proposal data received from coordinator (for distributed mode)
pub async fn sign_dns_proposals(
    config: &NodeConfig,
    dirs: &OnboardingDirs,
    proposal_data: &[u8],
) -> Result {
    sign_proposal(
        config,
        &dirs.dns_proposals_dir.join(DNS_PROTO_FILENAME),
        &dirs.dns_signed_dir,
        SIGNED_DNS_PROPOSAL_PREFIX,
        "DNS",
        Some(proposal_data),
    )
    .await
}

/// Sign P2P proposals with attestor's key
///
/// **Canton 3.4+**: Signing keys are now embedded in the P2P mapping.
/// This function signs the P2P proposal which contains both participant and key information.
///
/// This step must be run by each attestor participant (except the coordinator who created the proposals).
/// Each attestor signs the P2P proposal with their namespace key.
///
/// # Arguments
/// * `config` - Node configuration with Canton connection details
/// * `dirs` - Directory paths for the onboarding workflow
/// * `proposal_data` - Proposal data received from coordinator (for distributed mode)
pub async fn sign_p2p_proposals(
    config: &NodeConfig,
    dirs: &OnboardingDirs,
    proposal_data: &[u8],
) -> Result {
    sign_proposal(
        config,
        &dirs.p2p_proposals_dir.join(P2P_PROTO_FILENAME),
        &dirs.final_signed_dir,
        SIGNED_P2P_PROPOSALS_PREFIX,
        "P2P",
        Some(proposal_data),
    )
    .await
}
