use std::collections::HashSet;

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
        TopologyTransaction, enums::TopologyChangeOp, topology_mapping,
    },
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
        topology, topology_version,
    },
};

/// Submit kick to synchronizer.
///
/// The coordinator aggregates the per-peer signatures onto its own proposals
/// and submits the DNS mapping followed by the P2P mapping. A kicked owner's
/// mapping already exists before submission, so both post-submit polls require
/// the submitted serial and membership before legacy-key retirement can start.
pub async fn submit_kick(config: &NodeConfig, storage: &SqlitePool, instance_name: &str) -> Result {
    tracing::info!("Submitting kick to synchronizer...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let (mut dns_transaction, mut p2p_transaction) = topology::aggregate_dns_p2p_signatures(
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

    let current_namespace_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_NAMESPACE_DEF, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_NAMESPACE_DEF artifact missing"))?;
    let current_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&current_namespace_bytes)?;
    ensure_kick_signature_count(&dns_transaction, current_namespace_def.threshold, "DNS")?;
    ensure_kick_signature_count(&p2p_transaction, new_namespace_def.threshold, "P2P")?;

    let party_id_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_PARTY_ID, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_PARTY_ID artifact missing"))?;
    let party_id_raw = String::from_utf8(party_id_bytes)?.trim().to_string();
    let party_id = CantonId::parse(&party_id_raw)?;
    tracing::info!("Party ID: {party_id}");

    // Dedupe by signing fingerprint before anything else looks at the
    // transactions: a peer response can re-add the coordinator's own
    // signature. Canton drops duplicates rather than refusing them, so this is
    // about what the check below counts, not about being rejected on submit.
    topology::dedupe_signatures(&mut dns_transaction);
    topology::dedupe_signatures(&mut p2p_transaction);

    // The DNS is submitted first, so a P2P that Canton will refuse for a
    // missing signing-key signature has to stop the run before that happens.
    topology::check_added_signing_keys_signed(
        config,
        &synchronizer_id,
        &party_id,
        &p2p_transaction,
    )
    .await?;

    let dns_tx: TopologyTransaction =
        topology_version::decode_transaction(&dns_transaction.transaction)?;
    let p2p_tx: TopologyTransaction =
        topology_version::decode_transaction(&p2p_transaction.transaction)?;
    let Some(topology_mapping::Mapping::PartyToParticipant(expected_p2p)) =
        p2p_tx.mapping.and_then(|mapping| mapping.mapping)
    else {
        anyhow::bail!("Kick P2P proposal does not contain a PartyToParticipant mapping");
    };

    topology::submit_dns_then_p2p(
        config,
        &synchronizer_id,
        "kick",
        topology::DnsP2pSubmission {
            dns: dns_transaction,
            p2p: p2p_transaction,
            force_changes: topology::party_proposal_force_flags(),
        },
        || wait_for_dns_in_topology(config, &synchronizer_id, &new_namespace_def, dns_tx.serial),
        || wait_for_p2p_in_topology(config, &synchronizer_id, &expected_p2p, p2p_tx.serial),
    )
    .await?;

    tracing::info!("Kick submitted and confirmed successfully");
    Ok(())
}

/// A resumed run may predate the quorum checks. Reject a signature shortage
/// before either write, including both signature forms. Extra signatures do
/// not prove namespace authority; Canton still checks delegated signing keys.
fn ensure_kick_signature_count(
    transaction: &SignedTopologyTransaction,
    threshold: i32,
    label: &str,
) -> Result {
    let signed = transaction
        .signatures
        .iter()
        .chain(
            transaction
                .multi_transaction_signatures
                .iter()
                .flat_map(|multi| multi.signatures.iter()),
        )
        .map(|signature| signature.signed_by.as_str())
        .filter(|fingerprint| !fingerprint.is_empty())
        .collect::<HashSet<_>>()
        .len();
    if threshold < 1 || signed < threshold as usize {
        anyhow::bail!(
            "Kick {label} proposal carries {signed} distinct signatures, but requires at least \
             {threshold}. Refusing before changing the namespace."
        );
    }
    Ok(())
}

/// Wait for the submitted namespace membership to become effective.
async fn wait_for_dns_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    expected: &DecentralizedNamespaceDefinition,
    expected_serial: u32,
) -> Result {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());

    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ListDecentralizedNamespaceDefinitionRequest {
            base_query: Some(topology::head_state_query(synchronizer_id)),
            filter_namespace: expected.decentralized_namespace.clone(),
        });

        let response = topology_read_client
            .list_decentralized_namespace_definition(request)
            .await?
            .into_inner();

        if response.results.iter().any(|row| {
            row.context.as_ref().is_some_and(|context| {
                context.operation == TopologyChangeOp::AddReplace as i32
                    && i64::from(context.serial) >= i64::from(expected_serial)
            }) && row
                .item
                .as_ref()
                .is_some_and(|actual| dns_matches(actual, expected))
        }) {
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

/// Wait for the submitted participant and inline signing-key sets to become effective.
async fn wait_for_p2p_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    expected: &PartyToParticipant,
    expected_serial: u32,
) -> Result {
    let party_id_str = expected.party.clone();
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

        if topology::p2p_rows(response.results)?
            .iter()
            .any(|(context, actual)| {
                context.as_ref().is_some_and(|context| {
                    context.operation == TopologyChangeOp::AddReplace as i32
                        && i64::from(context.serial) >= i64::from(expected_serial)
                }) && p2p_matches(actual, expected)
            })
        {
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

fn dns_matches(
    actual: &DecentralizedNamespaceDefinition,
    expected: &DecentralizedNamespaceDefinition,
) -> bool {
    actual.decentralized_namespace == expected.decentralized_namespace
        && actual.threshold == expected.threshold
        && actual.owners.iter().collect::<HashSet<_>>()
            == expected.owners.iter().collect::<HashSet<_>>()
}

fn p2p_matches(actual: &PartyToParticipant, expected: &PartyToParticipant) -> bool {
    let mut actual_hosts = actual.participants.clone();
    let mut expected_hosts = expected.participants.clone();
    actual_hosts.sort_by(|a, b| a.participant_uid.cmp(&b.participant_uid));
    expected_hosts.sort_by(|a, b| a.participant_uid.cmp(&b.participant_uid));
    let keys_match = match (&actual.party_signing_keys, &expected.party_signing_keys) {
        (Some(actual), Some(expected)) => {
            actual.threshold == expected.threshold
                && actual
                    .keys
                    .iter()
                    .map(utils::compute_fingerprint)
                    .collect::<HashSet<_>>()
                    == expected
                        .keys
                        .iter()
                        .map(utils::compute_fingerprint)
                        .collect::<HashSet<_>>()
        }
        (None, None) => true,
        _ => false,
    };
    actual.party == expected.party
        && actual.threshold == expected.threshold
        && actual_hosts == expected_hosts
        && keys_match
}

#[cfg(test)]
mod tests {
    use super::*;
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::{Signature, SigningKeysWithThreshold, SigningPublicKey},
        protocol::v30::{MultiTransactionSignatures, party_to_participant::HostingParticipant},
    };

    fn signed_by(fingerprints: &[&str]) -> SignedTopologyTransaction {
        SignedTopologyTransaction {
            signatures: fingerprints
                .iter()
                .map(|fingerprint| Signature {
                    signed_by: (*fingerprint).to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_resumed_kick_cannot_raise_its_threshold_with_too_few_signatures() -> Result {
        let signed = signed_by(&["rotated-owner-1", "delegated-owner-2"]);
        ensure_kick_signature_count(&signed, 2, "DNS")?;
        let error = ensure_kick_signature_count(&signed, 3, "P2P")
            .expect_err("two signatures cannot meet the proposed threshold")
            .to_string();
        assert!(error.contains("Kick P2P"), "{error}");
        assert!(error.contains("2 distinct signatures"), "{error}");
        assert!(error.contains("at least 3"), "{error}");
        assert!(ensure_kick_signature_count(&signed, 3, "DNS").is_err());
        ensure_kick_signature_count(&signed, 2, "P2P")
    }

    #[test]
    fn duplicate_and_empty_fingerprints_cannot_fill_the_kick_quorum() {
        let signed = signed_by(&["owner-1", "owner-1", "", ""]);
        assert!(ensure_kick_signature_count(&signed, 2, "P2P").is_err());
    }

    #[test]
    fn the_kick_guard_counts_both_signature_forms_once_per_fingerprint() -> Result {
        let mut signed = signed_by(&["owner-1"]);
        signed.multi_transaction_signatures = vec![MultiTransactionSignatures {
            signatures: signed_by(&["owner-1", "delegated-owner-2", "rotated-owner-3"]).signatures,
            ..Default::default()
        }];
        ensure_kick_signature_count(&signed, 3, "P2P")?;
        assert!(ensure_kick_signature_count(&signed, 4, "P2P").is_err());
        Ok(())
    }

    #[test]
    fn post_kick_dns_rejects_old_authority_and_accepts_reordered_owners() {
        let expected = DecentralizedNamespaceDefinition {
            decentralized_namespace: "namespace".into(),
            threshold: 2,
            owners: vec!["one".into(), "two".into()],
        };
        let mut actual = expected.clone();
        actual.owners.reverse();
        assert!(dns_matches(&actual, &expected));
        actual.owners.push("kicked".into());
        assert!(!dns_matches(&actual, &expected));
        actual = expected.clone();
        actual.threshold = 1;
        assert!(!dns_matches(&actual, &expected));
        actual = expected.clone();
        actual.decentralized_namespace = "other".into();
        assert!(!dns_matches(&actual, &expected));
    }

    #[test]
    fn post_kick_p2p_requires_new_hosts_and_inline_keys() {
        let key = SigningPublicKey {
            public_key: vec![1; 32],
            ..Default::default()
        };
        let expected = PartyToParticipant {
            party: "party".into(),
            threshold: 1,
            participants: vec![HostingParticipant {
                participant_uid: "remaining".into(),
                permission: 2,
                onboarding: None,
            }],
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: vec![key.clone()],
                threshold: 1,
            }),
        };
        assert!(p2p_matches(&expected, &expected));
        let mut old = expected.clone();
        old.participants.push(HostingParticipant {
            participant_uid: "kicked".into(),
            permission: 2,
            onboarding: None,
        });
        assert!(!p2p_matches(&old, &expected));
        old = expected.clone();
        old.threshold = 2;
        assert!(!p2p_matches(&old, &expected));
        old = expected.clone();
        old.party_signing_keys = None;
        assert!(!p2p_matches(&old, &expected));
        old = expected.clone();
        old.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![SigningPublicKey {
                public_key: vec![2; 32],
                ..key
            }],
            threshold: 1,
        });
        assert!(!p2p_matches(&old, &expected));
    }
}
