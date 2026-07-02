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
    canton_id::CantonId,
    config::NodeConfig,
    consts::{
        TOPOLOGY_PROPAGATION_DELAY_SECS, topology_retry_delay_secs, topology_retry_max_attempts,
    },
    error::Result,
    utils,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

/// Submit the change-threshold proposals to the synchronizer.
///
/// The coordinator aggregates the per-peer signatures onto its own proposals
/// and submits the DNS mapping followed by the P2P mapping.
pub async fn submit_change(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
) -> Result {
    tracing::info!("Submitting change-threshold to synchronizer...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Read original DNS proposal
    let dns_bytes = storage
        .read_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_DNS_PROPOSAL,
            None,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("CHANGE_THRESHOLD_DNS_PROPOSAL artifact missing"))?;
    let mut dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&dns_bytes)?;

    // Read original P2P proposal
    let p2p_bytes = storage
        .read_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_P2P_PROPOSAL,
            None,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("CHANGE_THRESHOLD_P2P_PROPOSAL artifact missing"))?;
    let mut p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&p2p_bytes)?;

    // Gather per-peer signed proposals from storage, joining DNS and P2P by
    // peer id so the two signatures stay paired.
    let signed_dns = storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS)
        .await?;
    let signed_p2p: HashMap<String, Vec<u8>> = storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P)
        .await?
        .into_iter()
        .collect();

    tracing::info!(
        "Found signed proposals from {count} peer(s)",
        count = signed_dns.len()
    );

    if signed_dns.len() != signed_p2p.len() {
        anyhow::bail!(
            "Mismatched signed proposal counts: {dns} DNS vs {p2p} P2P",
            dns = signed_dns.len(),
            p2p = signed_p2p.len()
        );
    }

    // Aggregate signatures from each peer onto the coordinator's proposals.
    for (peer_id, dns_signed_bytes) in &signed_dns {
        tracing::info!("Reading signatures from peer {peer_id}");

        let dns_signed: SignedTopologyTransaction =
            utils::read_first_message_from_bytes(dns_signed_bytes)?;
        let p2p_signed_bytes = signed_p2p
            .get(peer_id)
            .ok_or_else(|| anyhow::anyhow!("Peer {peer_id} signed DNS but not P2P"))?;
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

    // Read new namespace definition for the topology-propagation poll.
    let new_namespace_bytes = storage
        .read_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_NEW_NAMESPACE_DEF,
            None,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("CHANGE_THRESHOLD_NEW_NAMESPACE_DEF artifact missing"))?;
    let new_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&new_namespace_bytes)?;

    // Read party ID
    let party_id_bytes = storage
        .read_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_PARTY_ID,
            None,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("CHANGE_THRESHOLD_PARTY_ID artifact missing"))?;
    let party_id_raw = String::from_utf8(party_id_bytes)?.trim().to_string();
    let party_id = CantonId::parse(&party_id_raw)?;
    tracing::info!("Party ID: {party_id}");

    // Submit DNS proposal first
    tracing::info!("Submitting DNS change-threshold proposal...");
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
    tracing::info!("DNS change-threshold proposal submitted");

    // Wait for the DNS to reflect the NEW threshold. The namespace already
    // exists (owners are unchanged), so we must poll on the threshold value
    // itself, not mere existence, or the check passes before the change lands.
    tracing::info!("Waiting for DNS change-threshold to take effect in topology...");
    wait_for_dns_in_topology(
        config,
        &synchronizer_id,
        &new_namespace_def.decentralized_namespace,
        new_namespace_def.threshold,
    )
    .await?;
    tracing::info!("DNS change-threshold confirmed in topology");

    // Submit P2P proposal
    tracing::info!("Submitting P2P change-threshold proposal...");
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
    tracing::info!("P2P change-threshold proposal submitted");

    // Wait for the P2P mapping to reflect the NEW threshold. The party id is
    // stable, so — like the DNS above — poll on the threshold value, not mere
    // existence.
    tracing::info!("Waiting for P2P change-threshold to take effect in topology...");
    wait_for_p2p_in_topology(
        config,
        &synchronizer_id,
        &party_id,
        new_namespace_def.threshold as u32,
    )
    .await?;
    tracing::info!("P2P change-threshold confirmed in topology");

    // Additional wait for topology propagation
    let propagation_delay = time::Duration::from_secs(TOPOLOGY_PROPAGATION_DELAY_SECS);
    tracing::info!("Waiting {propagation_delay:?} for Canton to propagate topology updates...");
    time::sleep(propagation_delay).await;

    tracing::info!("Change-threshold submitted and confirmed successfully");
    Ok(())
}

/// Wait until the namespace's head-state threshold equals `expected_threshold`.
///
/// A change-threshold run keeps the same owners, so the
/// `DecentralizedNamespaceDefinition` already exists before submission —
/// polling on existence alone would return immediately, before the new
/// threshold has propagated. We poll on the threshold value itself.
async fn wait_for_dns_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace: &str,
    expected_threshold: i32,
) -> Result {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());

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

        if response.results.iter().any(|r| {
            r.item
                .as_ref()
                .is_some_and(|d| d.threshold == expected_threshold)
        }) {
            tracing::info!(
                "DNS threshold {expected_threshold} confirmed in topology after {attempt} attempt(s)"
            );
            return Ok(());
        }

        if attempt < max_attempts {
            tracing::debug!(
                "DNS threshold not yet {expected_threshold}, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!(
        "DNS threshold did not become {expected_threshold} in topology after {max_attempts} attempts"
    )
}

/// Wait until the party mapping's head-state threshold equals
/// `expected_threshold`. Like the DNS poll, the mapping already exists (the
/// party id is stable), so we poll on the threshold value, not existence.
async fn wait_for_p2p_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
    expected_threshold: u32,
) -> Result {
    let party_id_str = party_id.to_string();
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());

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

        if response.results.iter().any(|r| {
            r.item
                .as_ref()
                .is_some_and(|p| p.threshold == expected_threshold)
        }) {
            tracing::info!(
                "P2P threshold {expected_threshold} confirmed in topology after {attempt} attempt(s)"
            );
            return Ok(());
        }

        if attempt < max_attempts {
            tracing::debug!(
                "P2P threshold not yet {expected_threshold}, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!(
        "P2P threshold did not become {expected_threshold} in topology after {max_attempts} attempts"
    )
}
