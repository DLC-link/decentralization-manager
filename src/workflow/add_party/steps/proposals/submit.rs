use std::collections::HashSet;

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
        DNS_ADD_PARTY_PROTO_FILENAME, NEW_NAMESPACE_DEF_FILENAME, P2P_ADD_PARTY_PROTO_FILENAME,
        PARTY_ID_FILENAME, SIGNED_ADD_PARTY_PROPOSALS_PREFIX, TOPOLOGY_PROPAGATION_DELAY_SECS,
        TOPOLOGY_RETRY_DELAY_SECS, TOPOLOGY_RETRY_MAX_ATTEMPTS,
    },
    error::Result,
    utils,
    workflow::add_party::AddPartyDirs,
};

/// Submit add party to synchronizer
///
/// Coordinator aggregates signatures and submits the add party
pub async fn submit_add_party(config: &NodeConfig, dirs: &AddPartyDirs) -> Result {
    tracing::info!("Submitting add party to synchronizer...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Read original DNS proposal
    let dns_file = dirs
        .add_party_proposals_dir
        .join(DNS_ADD_PARTY_PROTO_FILENAME);
    tracing::info!(
        "Reading original DNS proposal from {path}",
        path = dns_file.display()
    );
    let mut dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&dns_file).await?;
    tracing::info!(
        "Original DNS proposal has {count} signature(s) from coordinator",
        count = dns_transaction.signatures.len()
    );

    // Read original P2P proposal
    let p2p_file = dirs
        .add_party_proposals_dir
        .join(P2P_ADD_PARTY_PROTO_FILENAME);
    tracing::info!(
        "Reading original P2P proposal from {path}",
        path = p2p_file.display()
    );
    let mut p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_file(&p2p_file).await?;

    // Find all signed proposal files
    let signed_files = utils::find_files_by_pattern(
        &dirs.add_party_signed_dir,
        SIGNED_ADD_PARTY_PROPOSALS_PREFIX,
        ".bin",
    )
    .await?;
    tracing::info!(
        "Found {count} signed proposal file(s)",
        count = signed_files.len()
    );

    // Aggregate signatures from all files, deduplicating by signed_by
    // Track which keys have already signed to avoid duplicates
    let mut dns_signed_by: HashSet<String> = dns_transaction
        .signatures
        .iter()
        .map(|s| s.signed_by.clone())
        .collect();
    let mut p2p_signed_by: HashSet<String> = p2p_transaction
        .signatures
        .iter()
        .map(|s| s.signed_by.clone())
        .collect();

    for signed_file in &signed_files {
        tracing::info!(
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

        tracing::info!(
            "Attestor file has {dns_sigs} DNS signature(s) and {p2p_sigs} P2P signature(s)",
            dns_sigs = signed_transactions[0].signatures.len(),
            p2p_sigs = signed_transactions[1].signatures.len()
        );

        // First is DNS, second is P2P - only add signatures we haven't seen yet
        for sig in &signed_transactions[0].signatures {
            if dns_signed_by.insert(sig.signed_by.clone()) {
                tracing::debug!("Adding new DNS signature from {}", sig.signed_by);
                dns_transaction.signatures.push(sig.clone());
            } else {
                tracing::debug!("Skipping duplicate DNS signature from {}", sig.signed_by);
            }
        }
        for sig in &signed_transactions[1].signatures {
            if p2p_signed_by.insert(sig.signed_by.clone()) {
                tracing::debug!("Adding new P2P signature from {}", sig.signed_by);
                p2p_transaction.signatures.push(sig.clone());
            } else {
                tracing::debug!("Skipping duplicate P2P signature from {}", sig.signed_by);
            }
        }
    }

    tracing::info!(
        "Final DNS proposal has {count} unique signature(s)",
        count = dns_transaction.signatures.len()
    );
    for (i, sig) in dns_transaction.signatures.iter().enumerate() {
        tracing::info!("  DNS signature {i}: signed_by={}", sig.signed_by);
    }
    // Log the transaction bytes for debugging
    tracing::info!(
        "DNS transaction: {} bytes",
        dns_transaction.transaction.len()
    );
    tracing::info!(
        "Final P2P proposal has {count} unique signature(s)",
        count = p2p_transaction.signatures.len()
    );
    for (i, sig) in p2p_transaction.signatures.iter().enumerate() {
        tracing::info!("  P2P signature {i}: signed_by={}", sig.signed_by);
    }

    // Read new namespace definition for validation
    let new_namespace_file = dirs
        .add_party_proposals_dir
        .join(NEW_NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Reading new namespace definition from {path}",
        path = new_namespace_file.display()
    );
    let new_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&new_namespace_file).await?;

    // Expected counts after update (from the new namespace definition)
    let expected_owner_count = new_namespace_def.owners.len();
    // We're adding one participant, so expected = current + 1
    // The new_namespace_def.owners already includes the new member
    let expected_participant_count = expected_owner_count;
    tracing::info!(
        "Expected after update: {expected_owner_count} owners, {expected_participant_count} participants"
    );
    tracing::info!("New namespace owners:");
    for (i, owner) in new_namespace_def.owners.iter().enumerate() {
        tracing::info!("  Owner {i}: {owner}");
    }

    // Read party ID
    let party_id_file = dirs.add_party_proposals_dir.join(PARTY_ID_FILENAME);
    let party_id = tokio::fs::read_to_string(&party_id_file)
        .await?
        .trim()
        .to_string();
    tracing::info!("Party ID: {party_id}");

    // Submit DNS proposal first
    tracing::info!("Submitting DNS add party proposal...");
    let mut topology_write_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    // Use wait_to_become_effective to get feedback if the transaction doesn't become effective
    let dns_request = tonic::Request::new(AddTransactionsRequest {
        transactions: vec![dns_transaction],
        force_changes: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
            })),
        }),
        wait_to_become_effective: Some(prost_types::Duration {
            seconds: 30,
            nanos: 0,
        }),
    });

    let dns_result = topology_write_client.add_transactions(dns_request).await;
    match &dns_result {
        Ok(response) => {
            tracing::info!("DNS add party proposal submitted successfully");
            tracing::info!("DNS response (wait_to_become_effective): {:?}", response.get_ref());
        }
        Err(e) => {
            // Log the full error including any gRPC status details
            tracing::error!("DNS add party proposal rejected by Canton: {e}");
            tracing::error!("Full error details: {:?}", e);
            return Err(anyhow::anyhow!("Canton rejected DNS proposal: {e}"));
        }
    }

    // Wait for DNS to propagate with expected owner count
    tracing::info!("Waiting for DNS add party to appear in topology...");
    wait_for_dns_in_topology(
        config,
        &synchronizer_id,
        &new_namespace_def.decentralized_namespace,
        expected_owner_count,
    )
    .await?;
    tracing::info!("DNS add party confirmed in topology");

    // Submit P2P proposal
    tracing::info!("Submitting P2P add party proposal...");
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
    tracing::info!("P2P add party proposal submitted");

    // Wait for P2P to propagate with expected participant count
    tracing::info!("Waiting for P2P add party to appear in topology...");
    wait_for_p2p_in_topology(config, &synchronizer_id, &party_id, expected_participant_count).await?;
    tracing::info!("P2P add party confirmed in topology");

    // Additional wait for topology propagation
    let propagation_delay = time::Duration::from_secs(TOPOLOGY_PROPAGATION_DELAY_SECS);
    tracing::info!("Waiting {propagation_delay:?} for Canton to propagate topology updates...");
    time::sleep(propagation_delay).await;

    tracing::info!("Add party submitted and confirmed successfully");
    Ok(())
}

/// Wait for DNS with expected owner count to appear in topology by polling
async fn wait_for_dns_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace: &str,
    expected_owner_count: usize,
) -> Result {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let max_attempts = TOPOLOGY_RETRY_MAX_ATTEMPTS;
    let retry_delay = time::Duration::from_secs(TOPOLOGY_RETRY_DELAY_SECS);

    for attempt in 1..=max_attempts {
        // First check head state (effective transactions)
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

        if let Some(result) = response.results.first()
            && let Some(dns) = &result.item
        {
            let current_count = dns.owners.len();
            if current_count == expected_owner_count {
                tracing::info!(
                    "DNS with {current_count} owners found in topology after {attempt} attempt(s)"
                );
                return Ok(());
            }
            tracing::debug!(
                "DNS found but has {current_count} owners, expected {expected_owner_count}"
            );
        }

        // Also check proposals to see if it's pending
        if attempt == 1 || attempt == max_attempts {
            let proposals_request = tonic::Request::new(ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(BaseQuery {
                    store: Some(StoreId {
                        store: Some(store_id::Store::Synchronizer(Synchronizer {
                            kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.to_string())),
                        })),
                    }),
                    proposals: true,
                    operation: 0,
                    time_query: Some(base_query::TimeQuery::HeadState(())),
                    filter_signed_key: String::new(),
                    protocol_version: None,
                }),
                filter_namespace: namespace.to_string(),
            });

            let proposals_response = topology_read_client
                .list_decentralized_namespace_definition(proposals_request)
                .await?
                .into_inner();

            if !proposals_response.results.is_empty() {
                tracing::warn!(
                    "Found {count} DNS proposal(s) pending - may need more signatures or time to become effective",
                    count = proposals_response.results.len()
                );
                for (i, result) in proposals_response.results.iter().enumerate() {
                    if let Some(context) = &result.context {
                        tracing::warn!(
                            "  Proposal {i}: serial={}, valid_from={:?}, valid_until={:?}",
                            context.serial,
                            context.valid_from,
                            context.valid_until
                        );
                    }
                    if let Some(dns) = &result.item {
                        tracing::warn!(
                            "  Proposal {i}: threshold={}, owners={:?}",
                            dns.threshold,
                            dns.owners
                        );
                    }
                }
            }
        }

        if attempt < max_attempts {
            tracing::debug!(
                "DNS update not yet in topology, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!(
        "DNS with {expected_owner_count} owners did not appear in topology after {max_attempts} attempts"
    )
}

/// Wait for P2P with expected participant count to appear in topology by polling
async fn wait_for_p2p_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &str,
    expected_participant_count: usize,
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

        if let Some(result) = response.results.first()
            && let Some(p2p) = &result.item
        {
            let current_count = p2p.participants.len();
            if current_count == expected_participant_count {
                tracing::info!(
                    "P2P with {current_count} participants found in topology after {attempt} attempt(s)"
                );
                return Ok(());
            }
            tracing::debug!(
                "P2P found but has {current_count} participants, expected {expected_participant_count}"
            );
        }

        if attempt < max_attempts {
            tracing::debug!(
                "P2P update not yet in topology, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!(
        "P2P with {expected_participant_count} participants did not appear in topology after {max_attempts} attempts"
    )
}
