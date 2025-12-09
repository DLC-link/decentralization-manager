use tokio::time;

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{DecentralizedNamespaceDefinition, SignedTopologyTransaction},
    topology::admin::v30::{
        AddTransactionsRequest, BaseQuery, ListDecentralizedNamespaceDefinitionRequest,
        ListPartyToParticipantRequest, StoreId, Synchronizer, base_query, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{
        DNS_PROTO_FILENAME, NAMESPACE_DEF_FILENAME, P2P_PROTO_FILENAME, SIGNED_DNS_PROPOSAL_PREFIX,
        SIGNED_P2P_PROPOSALS_PREFIX, TOPOLOGY_PROPAGATION_DELAY_SECS, TOPOLOGY_RETRY_DELAY_SECS,
        TOPOLOGY_RETRY_MAX_ATTEMPTS,
    },
    error::Result,
    utils,
    workflow::onboarding::OnboardingDirs,
};

/// Aggregate and submit DNS proposals
///
/// This step must be run once by the coordinator after all attestors have signed the DNS proposal.
/// It aggregates all signatures and submits the fully-signed proposal to Canton.
pub async fn submit_dns_proposals(config: &NodeConfig, dirs: &OnboardingDirs) -> Result {
    tracing::info!("Submitting DNS proposals...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let dns_file = dirs.dns_proposals_dir.join(DNS_PROTO_FILENAME);
    tracing::info!(
        "Reading original DNS proposal from {path}",
        path = dns_file.display()
    );
    let mut dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&dns_file).await?;

    let signed_files =
        utils::find_files_by_pattern(&dirs.dns_signed_dir, SIGNED_DNS_PROPOSAL_PREFIX, ".bin")
            .await?;
    tracing::info!(
        "Found {count} signed DNS proposal files",
        count = signed_files.len()
    );

    for signed_file in &signed_files {
        tracing::info!(
            "Reading signatures from {path}",
            path = signed_file.display()
        );
        let signed_transactions: Vec<SignedTopologyTransaction> =
            utils::read_all_messages_from_file(signed_file).await?;

        for signed_tx in signed_transactions {
            dns_transaction
                .signatures
                .extend(signed_tx.signatures.clone());
        }
    }

    tracing::info!(
        "Aggregated DNS proposal has {count} signature(s)",
        count = dns_transaction.signatures.len()
    );

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

    let namespace_def_file = dirs.dns_submission_dir.join(NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Reading namespace definition from {path}",
        path = namespace_def_file.display()
    );
    let namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&namespace_def_file).await?;

    tracing::info!(
        "Waiting for DNS to appear in topology for namespace {namespace}...",
        namespace = namespace_def.decentralized_namespace
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
                operation: 0,
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
                "DNS not yet in topology, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!("DNS did not appear in topology after {max_attempts} attempts")
}

/// Aggregate and submit P2P proposals
///
/// **Canton 3.4+**: Submits P2P proposals with embedded signing keys
/// (replaces the separate PartyToKeyMapping transactions from Canton 3.3).
///
/// This step must be run once by the coordinator after all attestors have signed the P2P proposals.
/// It aggregates all signatures and submits the fully-signed proposal to Canton.
pub async fn submit_final_proposals(
    config: &NodeConfig,
    dirs: &OnboardingDirs,
    network_config: &NetworkConfig,
) -> Result {
    tracing::info!("Submitting P2P proposal with embedded signing keys (Canton 3.4+)...");

    let party_id_prefix = &network_config.application.party_id_prefix;

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let p2p_file = dirs.p2p_proposals_dir.join(P2P_PROTO_FILENAME);
    tracing::info!(
        "Reading original P2P proposal from {path}",
        path = p2p_file.display()
    );
    let mut p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&p2p_file).await?;

    let signed_files =
        utils::find_files_by_pattern(&dirs.final_signed_dir, SIGNED_P2P_PROPOSALS_PREFIX, ".bin")
            .await?;
    tracing::info!(
        "Found {count} signed P2P proposal files",
        count = signed_files.len()
    );

    for signed_file in &signed_files {
        tracing::info!(
            "Reading signatures from {path}",
            path = signed_file.display()
        );
        let signed_transactions: Vec<SignedTopologyTransaction> =
            utils::read_all_messages_from_file(signed_file).await?;

        if signed_transactions.len() != 1 {
            anyhow::bail!(
                "Expected 1 transaction in {path}, got {count}",
                path = signed_file.display(),
                count = signed_transactions.len()
            );
        }

        p2p_transaction
            .signatures
            .extend(signed_transactions[0].signatures.clone());
    }

    tracing::info!(
        "Aggregated P2P proposal has {count} signature(s)",
        count = p2p_transaction.signatures.len()
    );

    let namespace_file = dirs.dns_submission_dir.join(NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Reading namespace definition from {path}",
        path = namespace_file.display()
    );
    let namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&namespace_file).await?;

    let party_id = format!(
        "{party_id_prefix}::{namespace}",
        namespace = namespace_def.decentralized_namespace
    );
    tracing::info!("Constructed party ID: {party_id}");

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

    tracing::info!("Waiting for P2P to appear in topology...");
    let effective_time = wait_for_p2p_in_topology(config, &synchronizer_id, &party_id).await?;

    tracing::info!("P2P proposal submitted and confirmed in topology successfully");

    let now = std::time::SystemTime::now();
    let effective_system_time = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(effective_time.seconds as u64)
        + std::time::Duration::from_nanos(effective_time.nanos as u64);

    if let Ok(wait_duration) = effective_system_time.duration_since(now) {
        tracing::info!(
            "P2P mapping will become effective in {wait_duration:?}. Waiting for topology effective time..."
        );
        tokio::time::sleep(wait_duration).await;
        tracing::info!("Topology is now effective");
    } else {
        tracing::info!("P2P mapping is already effective");
    }

    let propagation_delay = time::Duration::from_secs(TOPOLOGY_PROPAGATION_DELAY_SECS);
    tracing::info!("Waiting {propagation_delay:?} for Canton to propagate topology updates...");
    time::sleep(propagation_delay).await;
    tracing::info!("Topology propagation wait complete");

    Ok(())
}

/// Wait for P2P (PartyToParticipant) to appear in topology by polling
/// Returns the effective time (valid_from) when the P2P mapping becomes active
async fn wait_for_p2p_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &str,
) -> Result<prost_types::Timestamp> {
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
                operation: 0,
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

        if let Some(result) = response.results.first() {
            tracing::info!("P2P found in topology after {attempt} attempt(s)");

            if let Some(context) = &result.context {
                if let Some(valid_from) = &context.valid_from {
                    tracing::debug!(
                        "P2P mapping effective time: {seconds}.{nanos:09}s",
                        seconds = valid_from.seconds,
                        nanos = valid_from.nanos
                    );
                    return Ok(*valid_from);
                } else {
                    anyhow::bail!("P2P mapping found but has no valid_from timestamp");
                }
            } else {
                anyhow::bail!("P2P mapping found but has no context");
            }
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
