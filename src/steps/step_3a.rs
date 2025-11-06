use std::path::Path;

use tokio::{fs, time};

use crate::{
    config::Config,
    error::Result,
    proto::com::digitalasset::canton::{
        protocol::v30::SignedTopologyTransaction,
        topology::admin::v30::{
            AddTransactionsRequest, BaseQuery, ListPartyToKeyMappingRequest,
            ListPartyToParticipantRequest, StoreId, Synchronizer, base_query, store_id,
            synchronizer, topology_manager_read_service_client::TopologyManagerReadServiceClient,
            topology_manager_write_service_client::TopologyManagerWriteServiceClient,
        },
    },
    utils,
};

/// Aggregate and submit P2P and PTK proposals
///
/// Corresponds to: 03a_SubmitFinalProposals.sc
///
/// This step must be run once by the coordinator after all attestors have signed the P2P and PTK proposals.
/// It aggregates all signatures and submits the fully-signed proposals to Canton.
///
/// # Arguments
/// * `config` - Configuration with Canton connection details
/// * `step_3_dir` - Directory containing p2p_proto.bin and ptk_proto.bin (usually ./out/step_3)
/// * `step_3a_dir` - Directory containing signed proposals (usually ./out/step_3a)
pub async fn submit_final_proposals(
    config: &Config,
    step_3_dir: &Path,
    step_3a_dir: &Path,
) -> Result {
    tracing::info!("Submitting final proposals...");

    // Step 1: Get synchronizer ID
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Step 2: Read the original P2P and PTK proposals
    let p2p_file = step_3_dir.join("p2p_proto.bin");
    tracing::info!("Reading original P2P proposal from {}", p2p_file.display());
    let mut p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&p2p_file).await?;

    let ptk_file = step_3_dir.join("ptk_proto.bin");
    tracing::info!("Reading original PTK proposal from {}", ptk_file.display());
    let mut ptk_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&ptk_file).await?;

    // Step 3: Discover and read all signed P2P/PTK proposals
    let signed_proposals_dir = step_3a_dir.join("signed-proposals");
    let mut signed_files = Vec::new();
    let mut dir_entries = fs::read_dir(&signed_proposals_dir).await?;

    while let Some(entry) = dir_entries.next_entry().await? {
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();
        if file_name_str.starts_with("signed-p2p-ptk-proposals")
            && file_name_str.ends_with(".bin")
        {
            signed_files.push(entry.path());
        }
    }

    signed_files.sort();
    tracing::info!("Found {} signed P2P/PTK proposal files", signed_files.len());

    // Step 4: Aggregate signatures separately for P2P and PTK
    // Each file contains 2 messages: P2P first, PTK second
    for signed_file in &signed_files {
        tracing::info!("Reading signatures from {}", signed_file.display());
        let signed_transactions: Vec<SignedTopologyTransaction> =
            utils::read_all_messages_from_file(signed_file).await?;

        if signed_transactions.len() != 2 {
            anyhow::bail!(
                "Expected 2 transactions in {}, got {}",
                signed_file.display(),
                signed_transactions.len()
            );
        }

        // First transaction is P2P
        p2p_transaction
            .signatures
            .extend(signed_transactions[0].signatures.clone());

        // Second transaction is PTK
        ptk_transaction
            .signatures
            .extend(signed_transactions[1].signatures.clone());
    }

    tracing::info!(
        "Aggregated P2P proposal has {} signature(s)",
        p2p_transaction.signatures.len()
    );
    tracing::info!(
        "Aggregated PTK proposal has {} signature(s)",
        ptk_transaction.signatures.len()
    );

    // Step 5: Submit P2P proposal first
    tracing::info!("Submitting aggregated P2P proposal...");
    let mut topology_write_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(AddTransactionsRequest {
        transactions: vec![p2p_transaction.clone()],
        force_changes: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::Id(synchronizer_id.clone())),
            })),
        }),
        wait_to_become_effective: None,
    });

    topology_write_client.add_transactions(request).await?;
    tracing::info!("P2P proposal submitted to topology");

    // Step 6: Wait for P2P to appear in topology
    tracing::info!("Waiting for P2P to appear in topology...");
    wait_for_p2p_in_topology(config, &synchronizer_id).await?;

    // Step 7: Submit PTK proposal second
    tracing::info!("Submitting aggregated PTK proposal...");
    let request = tonic::Request::new(AddTransactionsRequest {
        transactions: vec![ptk_transaction.clone()],
        force_changes: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::Id(synchronizer_id.clone())),
            })),
        }),
        wait_to_become_effective: None,
    });

    topology_write_client.add_transactions(request).await?;
    tracing::info!("PTK proposal submitted to topology");

    // Step 8: Wait for PTK to appear in topology
    tracing::info!("Waiting for PTK to appear in topology...");
    wait_for_ptk_in_topology(config, &synchronizer_id).await?;

    tracing::info!("Final proposals submitted and confirmed in topology successfully");
    Ok(())
}

/// Wait for P2P (PartyToParticipant) to appear in topology by polling
async fn wait_for_p2p_in_topology(config: &Config, synchronizer_id: &str) -> Result {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let max_attempts = 30;
    let retry_delay = time::Duration::from_secs(2);

    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(BaseQuery {
                store: Some(StoreId {
                    store: Some(store_id::Store::Synchronizer(Synchronizer {
                        kind: Some(synchronizer::Kind::Id(synchronizer_id.to_string())),
                    })),
                }),
                proposals: false,
                operation: 0, // TOPOLOGY_CHANGE_OP_UNSPECIFIED
                time_query: Some(base_query::TimeQuery::HeadState(())),
                filter_signed_key: String::new(),
                protocol_version: None,
            }),
            filter_party: String::new(),
            filter_participant: String::new(),
        });

        let response = topology_read_client
            .list_party_to_participant(request)
            .await?
            .into_inner();

        if !response.results.is_empty() {
            tracing::info!("P2P found in topology after {attempt} attempt(s)");
            return Ok(());
        }

        if attempt < max_attempts {
            tracing::debug!(
                "P2P not yet in topology, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!("P2P did not appear in topology after {max_attempts} attempts")
}

/// Wait for PTK (PartyToKeyMapping) to appear in topology by polling
async fn wait_for_ptk_in_topology(config: &Config, synchronizer_id: &str) -> Result {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let max_attempts = 30;
    let retry_delay = time::Duration::from_secs(2);

    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ListPartyToKeyMappingRequest {
            base_query: Some(BaseQuery {
                store: Some(StoreId {
                    store: Some(store_id::Store::Synchronizer(Synchronizer {
                        kind: Some(synchronizer::Kind::Id(synchronizer_id.to_string())),
                    })),
                }),
                proposals: false,
                operation: 0, // TOPOLOGY_CHANGE_OP_UNSPECIFIED
                time_query: Some(base_query::TimeQuery::HeadState(())),
                filter_signed_key: String::new(),
                protocol_version: None,
            }),
            filter_party: String::new(),
        });

        let response = topology_read_client
            .list_party_to_key_mapping(request)
            .await?
            .into_inner();

        if !response.results.is_empty() {
            tracing::info!("PTK found in topology after {attempt} attempt(s)");
            return Ok(());
        }

        if attempt < max_attempts {
            tracing::debug!(
                "PTK not yet in topology, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!("PTK did not appear in topology after {max_attempts} attempts")
}
