use crate::config::NodeConfig;
use tokio::{fs, time};

use crate::{
    consts::{TOPOLOGY_RETRY_DELAY_SECS, TOPOLOGY_RETRY_MAX_ATTEMPTS},
    dirs::WorkflowDirs,
    error::Result,
    proto::com::digitalasset::canton::{
        protocol::v30::{DecentralizedNamespaceDefinition, SignedTopologyTransaction},
        topology::admin::v30::{
            AddTransactionsRequest, BaseQuery, ListPartyToParticipantRequest, StoreId,
            Synchronizer, base_query, store_id, synchronizer,
            topology_manager_read_service_client::TopologyManagerReadServiceClient,
            topology_manager_write_service_client::TopologyManagerWriteServiceClient,
        },
    },
    utils,
};

/// Aggregate and submit P2P proposals
///
/// Corresponds to: 03a_SubmitFinalProposals.sc
///
/// **Canton 3.4+**: PTK (PartyToKeyMapping) is deprecated. This function only submits P2P proposals
/// which now contain embedded signing keys.
///
/// This step must be run once by the coordinator after all attestors have signed the P2P proposals.
/// It aggregates all signatures and submits the fully-signed proposal to Canton.
///
/// # Arguments
/// * `config` - Configuration with Canton connection details
/// * `dirs` - WorkflowDirs containing all directory paths
pub async fn submit_final_proposals(config: &NodeConfig, dirs: &WorkflowDirs) -> Result {
    tracing::info!("Submitting P2P proposal (Canton 3.4+: PTK deprecated, keys in P2P)...");

    // Step 1: Get synchronizer ID
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Step 2: Read the original P2P proposal
    // Canton 3.4+: PTK deprecated, only P2P proposal exists
    let p2p_file = dirs.p2p_ptk_proposals_dir.join("p2p_proto.bin");
    tracing::info!("Reading original P2P proposal from {}", p2p_file.display());
    let mut p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&p2p_file).await?;

    // Step 3: Discover and read all signed P2P proposals
    let signed_proposals_dir = &dirs.final_signed_dir;
    let mut signed_files = Vec::new();
    let mut dir_entries = fs::read_dir(&signed_proposals_dir).await?;

    while let Some(entry) = dir_entries.next_entry().await? {
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();
        if file_name_str.starts_with("signed-p2p-proposals") && file_name_str.ends_with(".bin") {
            signed_files.push(entry.path());
        }
    }

    signed_files.sort();
    tracing::info!("Found {} signed P2P proposal files", signed_files.len());

    // Step 4: Aggregate signatures for P2P
    // Canton 3.4+: Each file contains only 1 transaction (P2P only, PTK deprecated)
    for signed_file in &signed_files {
        tracing::info!("Reading signatures from {}", signed_file.display());
        let signed_transactions: Vec<SignedTopologyTransaction> =
            utils::read_all_messages_from_file(signed_file).await?;

        if signed_transactions.len() != 1 {
            anyhow::bail!(
                "Expected 1 transaction in {}, got {}",
                signed_file.display(),
                signed_transactions.len()
            );
        }

        // Aggregate P2P signatures
        p2p_transaction
            .signatures
            .extend(signed_transactions[0].signatures.clone());
    }

    tracing::info!(
        "Aggregated P2P proposal has {} signature(s)",
        p2p_transaction.signatures.len()
    );

    // Step 5: Read namespace definition and construct party ID
    let namespace_file = dirs.dns_submission_dir.join("namespaceDef.bin");
    tracing::info!(
        "Reading namespace definition from {}",
        namespace_file.display()
    );
    let namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&namespace_file).await?;

    let party_id = format!("cbtc-network::{}", namespace_def.decentralized_namespace);
    tracing::info!("Constructed party ID: {party_id}");

    // Step 6: Submit P2P proposal
    // Canton 3.4+: PTK deprecated, only submit P2P proposal
    tracing::info!("Submitting aggregated P2P proposal...");
    let mut topology_write_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(AddTransactionsRequest {
        transactions: vec![p2p_transaction.clone()],
        force_changes: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
            })),
        }),
        wait_to_become_effective: None,
    });

    topology_write_client.add_transactions(request).await?;
    tracing::info!("P2P proposal submitted to topology");

    // Step 7: Wait for P2P to appear in topology
    tracing::info!("Waiting for P2P to appear in topology...");
    wait_for_p2p_in_topology(config, &synchronizer_id, &party_id).await?;

    tracing::info!("P2P proposal submitted and confirmed in topology successfully");
    Ok(())
}

/// Wait for P2P (PartyToParticipant) to appear in topology by polling
async fn wait_for_p2p_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &str,
) -> Result {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let max_attempts = TOPOLOGY_RETRY_MAX_ATTEMPTS;
    let retry_delay = time::Duration::from_secs(TOPOLOGY_RETRY_DELAY_SECS);

    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(BaseQuery {
                store: Some(StoreId {
                    store: Some(store_id::Store::Synchronizer(Synchronizer {
                        kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.to_string())),
                    })),
                }),
                proposals: false,
                operation: 0, // TOPOLOGY_CHANGE_OP_UNSPECIFIED
                time_query: Some(base_query::TimeQuery::HeadState(())),
                filter_signed_key: String::new(),
                protocol_version: None,
            }),
            filter_party: party_id.to_string(),
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

