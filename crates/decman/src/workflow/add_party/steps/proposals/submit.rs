use canton_proto_rs::com::digitalasset::canton::protocol::v30::DecentralizedNamespaceDefinition;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        add_party::ARTIFACTS,
        add_party::AddPartyConfig,
        storage::{WorkflowStorage, artifact_kinds},
        topology::{self, PollText, Polled},
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
///
/// # Errors
///
/// Returns an error if the synchronizer ID cannot be read, or if a proposal
/// artefact or peer signature is missing, unreadable or unpaired. Returns an
/// error if a signing key the P2P adds has not signed it, or if a proposal's
/// serial moved since it was built. Returns an error if `AddTransactions`
/// fails, or if the new owners or the new participant do not appear in head
/// state within the polling budget.
pub async fn submit_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    tracing::info!("Submitting add-party proposals to synchronizer...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let (mut dns_transaction, mut p2p_transaction) =
        topology::aggregate_dns_p2p_signatures(storage, instance_name, ARTIFACTS).await?;

    // Dedupe by signing fingerprint: the coordinator's own signature is
    // already on the original proposals, and a retried peer may have signed
    // twice. Canton drops duplicates when it parses the transaction, so this
    // is about what the signature check below counts.
    topology::dedupe_signatures(&mut dns_transaction);
    topology::dedupe_signatures(&mut p2p_transaction);

    // The DNS is submitted first, so a P2P that Canton will refuse has to stop
    // the run here rather than half-way through it.
    topology::check_added_signing_keys_signed(
        config,
        &synchronizer_id,
        &add_party_config.decentralized_party_id,
        &p2p_transaction,
    )
    .await?;

    let confirm_p2p = || {
        wait_for_participant(
            config,
            &synchronizer_id,
            &add_party_config.decentralized_party_id,
            &add_party_config.new_participant_id,
        )
    };

    // A former host being hosted again leaves the namespace as it is; the DNS
    // the peers signed is the one already in force and is not resubmitted.
    let rehost = storage
        .read_artifact(instance_name, artifact_kinds::ADD_PARTY_REHOST, None)
        .await?
        .is_some();
    if rehost {
        tracing::info!("Namespace unchanged for a re-hosted member; submitting the P2P only");
        topology::submit_p2p(
            config,
            &synchronizer_id,
            "add-party",
            p2p_transaction,
            topology::party_proposal_force_flags(),
            confirm_p2p,
        )
        .await?;
    } else {
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
            topology::DnsP2pSubmission {
                dns: dns_transaction,
                p2p: p2p_transaction,
                force_changes: topology::party_proposal_force_flags(),
            },
            || {
                wait_for_owners(
                    config,
                    &synchronizer_id,
                    &new_namespace_def.decentralized_namespace,
                    &new_namespace_def.owners,
                )
            },
            confirm_p2p,
        )
        .await?;
    }

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
    let total = expected_owners.len();
    let text = PollText {
        found: format!("DNS owner set contains all {total} expected owners"),
        failed: format!("DNS owner set did not contain all {total} expected owners"),
    };
    topology::poll_until!(text, || async move {
        let namespace_def =
            topology::fetch_namespace_definition(config, synchronizer_id, namespace).await?;
        let present = expected_owners
            .iter()
            .filter(|owner| namespace_def.owners.contains(owner))
            .count();
        if present == total {
            return Ok(Polled::Ready(()));
        }
        Ok(Polled::Pending(format!(
            "DNS has {present}/{total} expected owners"
        )))
    })
    .await
}

/// Poll the synchronizer head state until `participant` shows up in the
/// party's P2P mapping.
async fn wait_for_participant(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
    participant: &CantonId,
) -> Result {
    let participant_str = participant.to_string();

    let text = PollText {
        found: format!("Participant {participant} present in P2P mapping"),
        failed: format!("Participant {participant} did not appear in the P2P mapping"),
    };
    topology::poll_until!(text, || {
        let participant_str = &participant_str;
        async move {
            let p2p = topology::fetch_p2p_mapping(config, synchronizer_id, party_id).await?;
            if p2p
                .participants
                .iter()
                .any(|p| p.participant_uid == *participant_str)
            {
                return Ok(Polled::Ready(()));
            }
            Ok(Polled::Pending(format!(
                "Participant {participant} not yet in P2P mapping"
            )))
        }
    })
    .await
}
