use std::collections::HashMap;

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{DecentralizedNamespaceDefinition, SignedTopologyTransaction},
    topology::admin::v30::{
        AddTransactionsRequest, BaseQuery, ListDecentralizedNamespaceDefinitionRequest,
        ListPartyToParticipantRequest, StoreId, Synchronizer, base_query, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use sqlx::SqlitePool;
use tokio::time;

use crate::{
    config::NodeConfig,
    consts::{
        TOPOLOGY_PROPAGATION_DELAY_SECS, TOPOLOGY_RETRY_DELAY_SECS, TOPOLOGY_RETRY_MAX_ATTEMPTS,
    },
    error::Result,
    participant_id::CantonId,
    utils,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

/// Submit kick to synchronizer
///
/// Coordinator aggregates signatures and submits the kick.
pub async fn submit_kick(config: &NodeConfig, storage: &SqlitePool, instance_name: &str) -> Result {
    tracing::info!("Submitting kick to synchronizer...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Read original DNS proposal
    let dns_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_DNS_PROPOSAL, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_DNS_PROPOSAL artifact missing"))?;
    let mut dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&dns_bytes)?;

    // Read original P2P proposal
    let p2p_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_P2P_PROPOSAL, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_P2P_PROPOSAL artifact missing"))?;
    let mut p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&p2p_bytes)?;

    // Gather per-attestor signed proposals from storage. We list both kinds
    // and join by attestor id so DNS and P2P signatures stay paired the way
    // the original combined-file format guaranteed.
    let signed_dns = storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_KICK_DNS)
        .await?;
    let signed_p2p: HashMap<String, Vec<u8>> = storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_KICK_P2P)
        .await?
        .into_iter()
        .collect();

    tracing::info!(
        "Found signed proposals from {count} attestor(s)",
        count = signed_dns.len()
    );

    if signed_dns.len() != signed_p2p.len() {
        anyhow::bail!(
            "Mismatched signed proposal counts: {dns} DNS vs {p2p} P2P",
            dns = signed_dns.len(),
            p2p = signed_p2p.len()
        );
    }

    // Aggregate signatures from each attestor
    for (attestor_id, dns_signed_bytes) in &signed_dns {
        tracing::info!("Reading signatures from attestor {attestor_id}");

        let dns_signed: SignedTopologyTransaction =
            utils::read_first_message_from_bytes(dns_signed_bytes)?;
        let p2p_signed_bytes = signed_p2p
            .get(attestor_id)
            .ok_or_else(|| anyhow::anyhow!("Attestor {attestor_id} signed DNS but not P2P"))?;
        let p2p_signed: SignedTopologyTransaction =
            utils::read_first_message_from_bytes(p2p_signed_bytes)?;

        dns_transaction.signatures.extend(dns_signed.signatures);
        p2p_transaction.signatures.extend(p2p_signed.signatures);
    }

    tracing::info!(
        "Final DNS proposal has {count} signature(s)",
        count = dns_transaction.signatures.len()
    );
    tracing::info!(
        "Final P2P proposal has {count} signature(s)",
        count = p2p_transaction.signatures.len()
    );

    // Read new namespace definition for validation
    let new_namespace_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_NEW_NAMESPACE_DEF, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_NEW_NAMESPACE_DEF artifact missing"))?;
    let new_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&new_namespace_bytes)?;

    // Read party ID
    let party_id_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_PARTY_ID, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_PARTY_ID artifact missing"))?;
    let party_id_raw = String::from_utf8(party_id_bytes)?.trim().to_string();
    let party_id = CantonId::parse(&party_id_raw)?;
    tracing::info!("Party ID: {party_id}");

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
    tracing::info!("DNS kick proposal submitted");

    // Wait for DNS to propagate
    tracing::info!("Waiting for DNS kick to appear in topology...");
    wait_for_dns_in_topology(
        config,
        &synchronizer_id,
        &new_namespace_def.decentralized_namespace,
    )
    .await?;
    tracing::info!("DNS kick confirmed in topology");

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
    tracing::info!("P2P kick proposal submitted");

    // Wait for P2P to propagate
    tracing::info!("Waiting for P2P kick to appear in topology...");
    wait_for_p2p_in_topology(config, &synchronizer_id, &party_id).await?;
    tracing::info!("P2P kick confirmed in topology");

    // Additional wait for topology propagation
    let propagation_delay = time::Duration::from_secs(TOPOLOGY_PROPAGATION_DELAY_SECS);
    tracing::info!("Waiting {propagation_delay:?} for Canton to propagate topology updates...");
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

/// Wait for P2P to appear in topology by polling
async fn wait_for_p2p_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result {
    let party_id_str = party_id.to_string();
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
            filter_party: party_id_str.clone(),
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
