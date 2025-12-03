use tokio::{fs, time};

use crate::{
    config::NodeConfig,
    consts::{
        DNS_PROTO_FILENAME, NAMESPACE_DEF_FILENAME, SIGNED_DNS_PROPOSAL_PREFIX,
        TOPOLOGY_RETRY_DELAY_SECS, TOPOLOGY_RETRY_MAX_ATTEMPTS,
    },
    dirs::WorkflowDirs,
    error::Result,
    proto::com::digitalasset::canton::{
        protocol::v30::{DecentralizedNamespaceDefinition, SignedTopologyTransaction},
        topology::admin::v30::{
            AddTransactionsRequest, BaseQuery, ListDecentralizedNamespaceDefinitionRequest,
            StoreId, Synchronizer, base_query, store_id, synchronizer,
            topology_manager_read_service_client::TopologyManagerReadServiceClient,
            topology_manager_write_service_client::TopologyManagerWriteServiceClient,
        },
    },
    utils,
};

/// Aggregate and submit DNS proposals
///
/// Corresponds to: 02a_SubmitProposals.sc
///
/// This step must be run once by the coordinator after all attestors have signed the DNS proposal.
/// It aggregates all signatures and submits the fully-signed proposal to Canton.
///
/// # Arguments
/// * `config` - Configuration with Canton connection details
/// * `dirs` - WorkflowDirs containing all directory paths
pub async fn submit_dns_proposals(config: &NodeConfig, dirs: &WorkflowDirs) -> Result {
    tracing::info!("Submitting DNS proposals...");

    // Step 1: Get synchronizer ID
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Step 2: Read the original DNS proposal
    let dns_file = dirs.dns_proposals_dir.join(DNS_PROTO_FILENAME);
    tracing::info!("Reading original DNS proposal from {}", dns_file.display());
    let mut dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&dns_file).await?;

    // Step 3: Discover and read all signed DNS proposals
    let signed_proposals_dir = &dirs.dns_signed_dir;
    let mut signed_files = Vec::new();
    let mut dir_entries = fs::read_dir(&signed_proposals_dir).await?;

    while let Some(entry) = dir_entries.next_entry().await? {
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();
        if file_name_str.starts_with(SIGNED_DNS_PROPOSAL_PREFIX) && file_name_str.ends_with(".bin")
        {
            signed_files.push(entry.path());
        }
    }

    signed_files.sort();
    tracing::info!("Found {} signed DNS proposal files", signed_files.len());

    // Step 4: Aggregate signatures from all signed proposals
    for signed_file in &signed_files {
        tracing::info!("Reading signatures from {}", signed_file.display());
        let signed_transactions: Vec<SignedTopologyTransaction> =
            utils::read_all_messages_from_file(signed_file).await?;

        for signed_tx in signed_transactions {
            // Merge signatures from this transaction into the main DNS transaction
            dns_transaction
                .signatures
                .extend(signed_tx.signatures.clone());
        }
    }

    tracing::info!(
        "Aggregated DNS proposal has {} signature(s)",
        dns_transaction.signatures.len()
    );

    // Step 5: Submit the aggregated DNS proposal to topology
    tracing::info!("Submitting aggregated DNS proposal...");
    let mut topology_write_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(AddTransactionsRequest {
        transactions: vec![dns_transaction],
        force_changes: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
            })),
        }),
        wait_to_become_effective: None,
    });

    topology_write_client.add_transactions(request).await?;
    tracing::info!("DNS proposal submitted to topology");

    // Step 6: Read namespace definition to get the namespace for polling
    let namespace_def_file = dirs.dns_submission_dir.join(NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Reading namespace definition from {}",
        namespace_def_file.display()
    );
    let namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&namespace_def_file).await?;

    // Step 7: Wait for DNS to appear in topology
    tracing::info!(
        "Waiting for DNS to appear in topology for namespace {}...",
        namespace_def.decentralized_namespace
    );
    wait_for_dns_in_topology(
        config,
        &synchronizer_id,
        &namespace_def.decentralized_namespace,
    )
    .await?;

    tracing::info!("DNS proposal submitted and confirmed in topology successfully");
    Ok(())
}

/// Wait for DNS to appear in topology by polling
async fn wait_for_dns_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace: &str,
) -> Result {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let max_attempts = TOPOLOGY_RETRY_MAX_ATTEMPTS;
    let retry_delay = time::Duration::from_secs(TOPOLOGY_RETRY_DELAY_SECS);

    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ListDecentralizedNamespaceDefinitionRequest {
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
            filter_namespace: namespace.to_string(),
        });

        let response = topology_read_client
            .list_decentralized_namespace_definition(request)
            .await?
            .into_inner();

        if !response.results.is_empty() {
            tracing::info!("DNS found in topology after {attempt} attempt(s)");
            return Ok(());
        }

        if attempt < max_attempts {
            tracing::debug!(
                "DNS not yet in topology, attempt {}/{}, retrying in {:?}...",
                attempt,
                max_attempts,
                retry_delay
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!(
        "DNS did not appear in topology after {} attempts",
        max_attempts
    )
}
