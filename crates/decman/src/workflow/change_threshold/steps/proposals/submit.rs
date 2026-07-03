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

/// Submit the change-threshold proposals to the synchronizer.
///
/// The coordinator aggregates the per-peer signatures onto its own proposals
/// and submits the DNS mapping followed by the P2P mapping. Both mappings
/// already exist (owners and participants are unchanged), so the post-submit
/// polls wait on the new threshold *value* rather than mere existence — else
/// the check passes before the change lands.
pub async fn submit_change(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
) -> Result {
    tracing::info!("Submitting change-threshold to synchronizer...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let (dns_transaction, p2p_transaction) = topology::aggregate_dns_p2p_signatures(
        storage,
        instance_name,
        topology::DnsP2pArtifactKinds {
            dns_proposal: artifact_kinds::CHANGE_THRESHOLD_DNS_PROPOSAL,
            p2p_proposal: artifact_kinds::CHANGE_THRESHOLD_P2P_PROPOSAL,
            signed_dns: artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            signed_p2p: artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
        },
    )
    .await?;

    // Read the new namespace definition (for the target threshold) + party id
    // needed by the post-submit topology polls.
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

    topology::submit_dns_then_p2p(
        config,
        &synchronizer_id,
        "change-threshold",
        dns_transaction,
        p2p_transaction,
        || {
            wait_for_dns_in_topology(
                config,
                &synchronizer_id,
                &new_namespace_def.decentralized_namespace,
                new_namespace_def.threshold,
            )
        },
        || {
            wait_for_p2p_in_topology(
                config,
                &synchronizer_id,
                &party_id,
                new_namespace_def.threshold as u32,
            )
        },
    )
    .await?;

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
            base_query: Some(topology::head_state_query(synchronizer_id)),
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
            base_query: Some(topology::head_state_query(synchronizer_id)),
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
