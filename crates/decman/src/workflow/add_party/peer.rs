use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::party_to_participant::HostingParticipant,
    topology::admin::v30::{
        ListPartyToParticipantRequest, list_party_to_participant_response::result::Item as P2pItem,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    noise::client::NoiseClient,
    utils,
    workflow::{
        storage::{WorkflowStorage, artifact_kinds},
        topology,
    },
};

/// Wait until this node's own synchronizer store holds the P2P proposal that
/// hosts it with the onboarding marker.
///
/// `ImportPartyAcs` checks the target's own store for such a mapping, effective
/// or proposed, and refuses the import otherwise. A synchronizer store is fed
/// only by sequenced transactions, so the coordinator's proposal reaches it a
/// moment after `Authorize` returned on the coordinator. The new member
/// disconnects before the mapping is authorized and will not see it take
/// effect, so the proposal has to be in its store before it disconnects.
///
/// # Errors
/// Returns an error if the proposal has not arrived within the topology retry
/// budget.
pub async fn wait_for_hosting_proposal(config: &NodeConfig, party_id: &CantonId) -> Result {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let self_id = config.participant_id().to_string();
    let max_attempts = topology_retry_max_attempts();
    let retry_delay = std::time::Duration::from_secs(topology_retry_delay_secs());
    let hosts_self =
        |p: &HostingParticipant| p.participant_uid == self_id && p.onboarding.is_some();

    for attempt in 1..=max_attempts {
        let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?);
        for proposals in [true, false] {
            let mut base_query = topology::head_state_query(&synchronizer_id);
            base_query.proposals = proposals;
            let response = client
                .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
                    base_query: Some(base_query),
                    filter_party: party_id.to_string(),
                    filter_participant: String::new(),
                }))
                .await?
                .into_inner();
            let found = response.results.into_iter().any(|r| {
                let Some(P2pItem::V30(mapping)) = r.item else {
                    return false;
                };
                mapping.party == party_id.to_string() && mapping.participants.iter().any(hosts_self)
            });
            if found {
                tracing::info!(
                    "This node's store holds the mapping hosting it with the onboarding marker \
                     (proposal: {proposals}) after {attempt} attempt(s)"
                );
                return Ok(());
            }
        }
        if attempt < max_attempts {
            tokio::time::sleep(retry_delay).await;
        }
    }
    anyhow::bail!(
        "the P2P proposal hosting {self_id} for {party_id} did not reach this node's store \
         within {max_attempts} attempts"
    )
}

/// Status string a non-addressed peer replies with when a new-member-only
/// command (GenerateAddPartyKeys / ImportAcs / ClearOnboardingFlag) isn't for
/// it. Any status completes the peer for the step — the constant just keeps
/// the coordinator logs readable.
pub const SKIP_STATUS: &[u8] = b"skipped (not the new member)";

/// Send the new member's `keys||participant_id` blob to the coordinator —
/// same two-item length-prefixed payload onboarding peers send, so the
/// coordinator's split-and-save path is shared.
pub async fn send_keys_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let self_id = node_config.participant_id().to_string();

    let keys_data = storage
        .read_artifact(
            instance_name,
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&self_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("PEER_PUBLIC_KEYS artifact missing for {self_id}"))?;

    let id_data = storage
        .read_artifact(
            instance_name,
            artifact_kinds::PARTICIPANT_ID,
            Some(&self_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("PARTICIPANT_ID artifact missing for {self_id}"))?;

    let combined_payload = crate::utils::encode_length_prefixed(&[&keys_data, &id_data]);
    client.upload_add_party_keys(combined_payload).await?;
    Ok(())
}

/// Send this peer's signed DNS + P2P add-party proposals to the coordinator
/// as one concatenated buffer (two `varint(len)||proto` blobs back to back),
/// mirroring the kick signature wire format.
pub async fn send_add_party_signatures_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let node_id = node_config.participant_id().to_string();

    let dns = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_ADD_PARTY_DNS artifact missing for {node_id}"))?;
    let p2p = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_P2P,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_ADD_PARTY_P2P artifact missing for {node_id}"))?;

    let mut payload = Vec::with_capacity(dns.len() + p2p.len());
    payload.extend_from_slice(&dns);
    payload.extend_from_slice(&p2p);

    client.send_add_party_signatures(payload).await?;
    Ok(())
}

/// Send this peer's signed onboarding-flag clearing proposal (a single
/// `varint(len)||proto` blob) to the coordinator.
pub async fn send_clear_signature_to_coordinator(
    client: &NoiseClient,
    storage: &SqlitePool,
    instance_name: &str,
    node_config: &NodeConfig,
) -> Result {
    let node_id = node_config.participant_id().to_string();

    let data = storage
        .read_artifact(
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
            Some(&node_id),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("SIGNED_ADD_PARTY_CLEAR artifact missing for {node_id}"))?;

    client.send_add_party_clear_signature(data).await?;
    Ok(())
}
