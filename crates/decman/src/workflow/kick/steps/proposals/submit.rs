use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::DecentralizedNamespaceDefinition,
    topology::admin::v30::{
        ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use sqlx::SqlitePool;
use tokio::time;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    utils,
    workflow::{
        storage::{WorkflowStorage, artifact_kinds},
        topology,
    },
};

/// Submit kick to synchronizer.
///
/// The coordinator aggregates the per-peer signatures onto its own proposals
/// and submits the DNS mapping followed by the P2P mapping. A kicked owner's
/// mapping briefly disappears from the head state, so both post-submit polls
/// wait for mere existence of the updated mapping.
pub async fn submit_kick(config: &NodeConfig, storage: &SqlitePool, instance_name: &str) -> Result {
    tracing::info!("Submitting kick to synchronizer...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let (dns_transaction, p2p_transaction) = topology::aggregate_dns_p2p_signatures(
        storage,
        instance_name,
        topology::DnsP2pArtifactKinds {
            dns_proposal: artifact_kinds::KICK_DNS_PROPOSAL,
            p2p_proposal: artifact_kinds::KICK_P2P_PROPOSAL,
            signed_dns: artifact_kinds::SIGNED_KICK_DNS,
            signed_p2p: artifact_kinds::SIGNED_KICK_P2P,
        },
    )
    .await?;

    // Read the new namespace definition + party id needed by the post-submit
    // topology polls.
    let new_namespace_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_NEW_NAMESPACE_DEF, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_NEW_NAMESPACE_DEF artifact missing"))?;
    let new_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&new_namespace_bytes)?;

    let party_id_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_PARTY_ID, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_PARTY_ID artifact missing"))?;
    let party_id_raw = String::from_utf8(party_id_bytes)?.trim().to_string();
    let party_id = CantonId::parse(&party_id_raw)?;
    tracing::info!("Party ID: {party_id}");

    topology::submit_dns_then_p2p(
        config,
        &synchronizer_id,
        "kick",
        dns_transaction,
        p2p_transaction,
        || {
            wait_for_dns_in_topology(
                config,
                &synchronizer_id,
                &new_namespace_def.decentralized_namespace,
            )
        },
        || wait_for_p2p_in_topology(config, &synchronizer_id, &party_id),
    )
    .await?;

    tracing::info!("Kick submitted and confirmed successfully");
    Ok(())
}

/// Wait for the decentralized namespace to appear in the topology head state.
async fn wait_for_dns_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace: &str,
) -> Result {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());

    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ListDecentralizedNamespaceDefinitionRequest {
            base_query: Some(topology::head_state_query(synchronizer_id)),
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

/// Wait for the party's P2P mapping to appear in the topology head state.
async fn wait_for_p2p_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result {
    let party_id_str = party_id.to_string();
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());

    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(topology::head_state_query(synchronizer_id)),
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
