use std::collections::{HashMap, HashSet};

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{DecentralizedNamespaceDefinition, SignedTopologyTransaction},
    topology::admin::v30::{
        AddTransactionsRequest, StoreId, Synchronizer, store_id, synchronizer,
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
    workflow::{
        add_party::{
            AddPartyConfig,
            steps::export_state::{fetch_namespace_definition, fetch_p2p_mapping},
        },
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Coordinator step: aggregate the peers' signatures onto the original
/// proposals and submit them — DNS first, then P2P — waiting after each for
/// the updated mapping to land in the synchronizer head state.
///
/// Unlike kick (where polling for mere existence suffices because the
/// mapping briefly disappears), both mappings already exist here, so the
/// waits check the COUNTS: DNS until the owner set has grown to the new
/// size, P2P until the new participant appears.
pub async fn submit_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    tracing::info!("Submitting add-party proposals to synchronizer...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let dns_bytes = storage
        .read_artifact(instance_name, artifact_kinds::ADD_PARTY_DNS_PROPOSAL, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("ADD_PARTY_DNS_PROPOSAL artifact missing"))?;
    let mut dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&dns_bytes)?;

    let p2p_bytes = storage
        .read_artifact(instance_name, artifact_kinds::ADD_PARTY_P2P_PROPOSAL, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("ADD_PARTY_P2P_PROPOSAL artifact missing"))?;
    let mut p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&p2p_bytes)?;

    let signed_dns = storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_ADD_PARTY_DNS)
        .await?;
    let signed_p2p: HashMap<String, Vec<u8>> = storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_ADD_PARTY_P2P)
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

    for (peer_id, dns_signed_bytes) in &signed_dns {
        tracing::info!("Aggregating signatures from peer {peer_id}");
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

    // Dedupe by signing fingerprint: the coordinator's own signature is
    // already on the original proposals, and a retried peer may have signed
    // twice. Canton rejects duplicate signatures on a submitted transaction.
    dedupe_signatures(&mut dns_transaction);
    dedupe_signatures(&mut p2p_transaction);

    tracing::info!(
        "Final DNS proposal has {dns} signature(s), P2P has {p2p}",
        dns = dns_transaction.signatures.len(),
        p2p = p2p_transaction.signatures.len()
    );

    let new_namespace_bytes = storage
        .read_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_NEW_NAMESPACE_DEF,
            None,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("ADD_PARTY_NEW_NAMESPACE_DEF artifact missing"))?;
    let new_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&new_namespace_bytes)?;

    let mut topology_write_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    tracing::info!("Submitting DNS add-party proposal...");
    topology_write_client
        .add_transactions(tonic::Request::new(add_transactions_request(
            &synchronizer_id,
            dns_transaction,
        )))
        .await?;

    tracing::info!("Waiting for the grown DNS owner set to appear in topology...");
    wait_for_owners(
        config,
        &synchronizer_id,
        &new_namespace_def.decentralized_namespace,
        &new_namespace_def.owners,
    )
    .await?;

    tracing::info!("Submitting P2P add-party proposal...");
    topology_write_client
        .add_transactions(tonic::Request::new(add_transactions_request(
            &synchronizer_id,
            p2p_transaction,
        )))
        .await?;

    tracing::info!("Waiting for the new participant to appear in the P2P mapping...");
    wait_for_participant(
        config,
        &synchronizer_id,
        &add_party_config.decentralized_party_id,
        &add_party_config.new_participant_id,
    )
    .await?;

    let propagation_delay = time::Duration::from_secs(TOPOLOGY_PROPAGATION_DELAY_SECS);
    tracing::info!("Waiting {propagation_delay:?} for Canton to propagate topology updates...");
    time::sleep(propagation_delay).await;

    tracing::info!("Add-party proposals submitted and confirmed successfully");
    Ok(())
}

/// Drop duplicate signatures, keeping the first per signing fingerprint.
pub(crate) fn dedupe_signatures(transaction: &mut SignedTopologyTransaction) {
    let mut seen = HashSet::new();
    transaction
        .signatures
        .retain(|sig| seen.insert(sig.signed_by.clone()));
}

pub(crate) fn add_transactions_request(
    synchronizer_id: &str,
    transaction: SignedTopologyTransaction,
) -> AddTransactionsRequest {
    AddTransactionsRequest {
        transactions: vec![transaction],
        force_changes: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.to_string())),
            })),
        }),
        wait_to_become_effective: None,
    }
}

/// Poll the synchronizer head state until the decentralized namespace lists
/// every expected owner.
///
/// Checks the owner set by identity rather than by size: a bare count could be
/// satisfied prematurely by an unrelated concurrent owner change, whereas the
/// add only succeeds once the new member's fingerprint is actually present.
async fn wait_for_owners(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace: &str,
    expected_owners: &[String],
) -> Result {
    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());

    for attempt in 1..=max_attempts {
        let namespace_def = fetch_namespace_definition(config, synchronizer_id, namespace).await?;
        let present = expected_owners
            .iter()
            .filter(|owner| namespace_def.owners.contains(owner))
            .count();
        if present == expected_owners.len() {
            tracing::info!(
                "DNS owner set contains all {total} expected owners after {attempt} attempt(s)",
                total = expected_owners.len()
            );
            return Ok(());
        }
        if attempt < max_attempts {
            tracing::debug!(
                "DNS has {present}/{total} expected owners, attempt \
                 {attempt}/{max_attempts}, retrying in {retry_delay:?}...",
                total = expected_owners.len()
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!(
        "DNS owner set did not contain all {total} expected owners after {max_attempts} attempts",
        total = expected_owners.len()
    )
}

/// Poll the synchronizer head state until `participant` shows up in the
/// party's P2P mapping.
async fn wait_for_participant(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
    participant: &CantonId,
) -> Result {
    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());
    let participant_str = participant.to_string();

    for attempt in 1..=max_attempts {
        let p2p = fetch_p2p_mapping(config, synchronizer_id, party_id).await?;
        if p2p
            .participants
            .iter()
            .any(|p| p.participant_uid == participant_str)
        {
            tracing::info!(
                "Participant {participant} present in P2P mapping after {attempt} attempt(s)"
            );
            return Ok(());
        }
        if attempt < max_attempts {
            tracing::debug!(
                "Participant {participant} not yet in P2P mapping, attempt \
                 {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!(
        "Participant {participant} did not appear in the P2P mapping after \
         {max_attempts} attempts"
    )
}
