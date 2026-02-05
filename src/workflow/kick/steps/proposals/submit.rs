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
    config::NodeConfig,
    consts::{
        DNS_KICK_PROTO_FILENAME, NEW_NAMESPACE_DEF_FILENAME, P2P_KICK_PROTO_FILENAME,
        PARTY_ID_FILENAME, SIGNED_KICK_PROPOSALS_PREFIX, TOPOLOGY_PROPAGATION_DELAY_SECS,
        TOPOLOGY_RETRY_DELAY_SECS, TOPOLOGY_RETRY_MAX_ATTEMPTS,
    },
    error::Result,
    utils,
    workflow::kick::KickDirs,
};

/// Submit kick to synchronizer
///
/// Coordinator aggregates signatures and submits the kick
pub async fn submit_kick(config: &NodeConfig, dirs: &KickDirs) -> Result {
    tracing::info!("Submitting kick to synchronizer...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Read original DNS proposal
    let dns_file = dirs.kick_proposals_dir.join(DNS_KICK_PROTO_FILENAME);
    tracing::debug!(
        "Reading original DNS proposal from {path}",
        path = dns_file.display()
    );
    let mut dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&dns_file).await?;

    // Read original P2P proposal
    let p2p_file = dirs.kick_proposals_dir.join(P2P_KICK_PROTO_FILENAME);
    tracing::debug!(
        "Reading original P2P proposal from {path}",
        path = p2p_file.display()
    );
    let mut p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&p2p_file).await?;

    // Find all signed proposal files
    let signed_files =
        utils::find_files_by_pattern(&dirs.kick_signed_dir, SIGNED_KICK_PROPOSALS_PREFIX, ".bin")
            .await?;
    tracing::debug!(
        "Found {count} signed proposal file(s)",
        count = signed_files.len()
    );

    // Aggregate signatures from all files
    for signed_file in &signed_files {
        tracing::debug!(
            "Reading signatures from {path}",
            path = signed_file.display()
        );
        let signed_transactions: Vec<SignedTopologyTransaction> =
            utils::read_all_messages_from_file(signed_file).await?;

        if signed_transactions.len() != 2 {
            anyhow::bail!(
                "Expected 2 transactions in {path}, got {count}",
                path = signed_file.display(),
                count = signed_transactions.len()
            );
        }

        // First is DNS, second is P2P
        dns_transaction
            .signatures
            .extend(signed_transactions[0].signatures.clone());
        p2p_transaction
            .signatures
            .extend(signed_transactions[1].signatures.clone());
    }

    tracing::debug!(
        "Final DNS proposal has {count} signature(s)",
        count = dns_transaction.signatures.len()
    );
    tracing::debug!(
        "Final P2P proposal has {count} signature(s)",
        count = p2p_transaction.signatures.len()
    );

    // Read new namespace definition for validation
    let new_namespace_file = dirs.kick_proposals_dir.join(NEW_NAMESPACE_DEF_FILENAME);
    tracing::debug!(
        "Reading new namespace definition from {path}",
        path = new_namespace_file.display()
    );
    let new_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&new_namespace_file).await?;

    // Read party ID
    let party_id_file = dirs.kick_proposals_dir.join(PARTY_ID_FILENAME);
    let party_id = tokio::fs::read_to_string(&party_id_file)
        .await?
        .trim()
        .to_string();
    tracing::debug!("Party ID: {party_id}");

    // Submit DNS proposal first
    tracing::info!("Submitting DNS kick proposal...");
    let mut topology_write_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let dns_request = tonic::Request::new(AddTransactionsRequest {
        transactions: vec![dns_transaction],
        force_changes: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
            })),
        }),
        wait_to_become_effective: None,
    });

    topology_write_client.add_transactions(dns_request).await?;
    tracing::debug!("DNS kick proposal submitted");

    // Wait for DNS to propagate
    tracing::debug!("Waiting for DNS kick to appear in topology...");
    wait_for_dns_in_topology(
        config,
        &synchronizer_id,
        &new_namespace_def.decentralized_namespace,
    )
    .await?;
    tracing::debug!("DNS kick confirmed in topology");

    // Submit P2P proposal
    tracing::info!("Submitting P2P kick proposal...");
    let p2p_request = tonic::Request::new(AddTransactionsRequest {
        transactions: vec![p2p_transaction],
        force_changes: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
            })),
        }),
        wait_to_become_effective: None,
    });

    topology_write_client.add_transactions(p2p_request).await?;
    tracing::debug!("P2P kick proposal submitted");

    // Wait for P2P to propagate
    tracing::debug!("Waiting for P2P kick to appear in topology...");
    wait_for_p2p_in_topology(config, &synchronizer_id, &party_id).await?;
    tracing::debug!("P2P kick confirmed in topology");

    // Additional wait for topology propagation
    let propagation_delay = time::Duration::from_secs(TOPOLOGY_PROPAGATION_DELAY_SECS);
    tracing::debug!("Waiting {propagation_delay:?} for Canton to propagate topology updates...");
    time::sleep(propagation_delay).await;

    tracing::info!("Kick submitted and confirmed successfully");
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
            tracing::debug!("DNS found in topology after {attempt} attempt(s)");
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

/// Wait for P2P to appear in topology by polling
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

        if !response.results.is_empty() {
            tracing::debug!("P2P found in topology after {attempt} attempt(s)");
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
