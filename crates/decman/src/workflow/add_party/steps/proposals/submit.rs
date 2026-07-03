use canton_proto_rs::com::digitalasset::canton::protocol::v30::DecentralizedNamespaceDefinition;
use sqlx::SqlitePool;
use tokio::time;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    utils,
    workflow::{
        add_party::AddPartyConfig,
        storage::{WorkflowStorage, artifact_kinds},
        topology,
    },
};

/// Coordinator step: aggregate the peers' signatures onto the original
/// proposals and submit them — DNS first, then P2P — waiting after each for
/// the updated mapping to land in the synchronizer head state.
///
/// Unlike kick (where polling for mere existence suffices because the mapping
/// briefly disappears), both mappings already exist here, so the waits check
/// membership: DNS until the new member's fingerprint joins the owner set, P2P
/// until the new participant appears.
pub async fn submit_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    tracing::info!("Submitting add-party proposals to synchronizer...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let (mut dns_transaction, mut p2p_transaction) = topology::aggregate_dns_p2p_signatures(
        storage,
        instance_name,
        topology::DnsP2pArtifactKinds {
            dns_proposal: artifact_kinds::ADD_PARTY_DNS_PROPOSAL,
            p2p_proposal: artifact_kinds::ADD_PARTY_P2P_PROPOSAL,
            signed_dns: artifact_kinds::SIGNED_ADD_PARTY_DNS,
            signed_p2p: artifact_kinds::SIGNED_ADD_PARTY_P2P,
        },
    )
    .await?;

    // Dedupe by signing fingerprint: the coordinator's own signature is
    // already on the original proposals, and a retried peer may have signed
    // twice. Canton rejects duplicate signatures on a submitted transaction.
    topology::dedupe_signatures(&mut dns_transaction);
    topology::dedupe_signatures(&mut p2p_transaction);

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

    topology::submit_dns_then_p2p(
        config,
        &synchronizer_id,
        "add-party",
        dns_transaction,
        p2p_transaction,
        || {
            wait_for_owners(
                config,
                &synchronizer_id,
                &new_namespace_def.decentralized_namespace,
                &new_namespace_def.owners,
            )
        },
        || {
            wait_for_participant(
                config,
                &synchronizer_id,
                &add_party_config.decentralized_party_id,
                &add_party_config.new_participant_id,
            )
        },
    )
    .await?;

    tracing::info!("Add-party proposals submitted and confirmed successfully");
    Ok(())
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
        let namespace_def =
            topology::fetch_namespace_definition(config, synchronizer_id, namespace).await?;
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
        let p2p = topology::fetch_p2p_mapping(config, synchronizer_id, party_id).await?;
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
