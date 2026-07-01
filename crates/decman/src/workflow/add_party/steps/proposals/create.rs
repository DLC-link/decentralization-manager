use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::SigningKeysWithThreshold,
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, TopologyMapping, enums,
        party_to_participant::{HostingParticipant, hosting_participant},
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, ForceFlag, StoreId, Synchronizer, authorize_request, store_id,
        synchronizer,
    },
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        add_party::{
            AddPartyConfig,
            steps::export_state::{encode_length_prefixed_message, fetch_p2p_mapping},
        },
        onboarding::steps::proposals::create::decode_keys_payload,
        storage::{WorkflowStorage, artifact_kinds},
        topology::authorize_with_topology_retry,
    },
};

/// Coordinator step: build and propose the updated topology for the add.
///
/// Creates:
/// - new `DecentralizedNamespaceDefinition`: same namespace hash, existing
///   owners + the new member's namespace fingerprint, the new threshold —
///   persisted as `ADD_PARTY_DNS_PROPOSAL` (+ `ADD_PARTY_NEW_NAMESPACE_DEF`
///   for submit's propagation wait)
/// - new `PartyToParticipant`: existing participants + the new member with
///   `Confirmation` permission and the **Onboarding marker** set (suspends
///   the party on the new member until the ACS import lands and the flag is
///   cleared), party signing keys merged with the new member's DAML key —
///   persisted as `ADD_PARTY_P2P_PROPOSAL`
pub async fn create_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    tracing::info!("Creating add-party proposals...");

    let namespace_bytes = storage
        .read_artifact(instance_name, artifact_kinds::ADD_PARTY_NAMESPACE_DEF, None)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("ADD_PARTY_NAMESPACE_DEF artifact missing — did ExportState run?")
        })?;
    let current_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&namespace_bytes)?;

    let new_member_id = add_party_config.new_participant_id.to_string();
    let keys_payload = storage
        .read_artifact(
            instance_name,
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&new_member_id),
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "PEER_PUBLIC_KEYS artifact missing for new member {new_member_id} — \
                 did GenerateNewMemberKeys run?"
            )
        })?;
    let keys = decode_keys_payload(&keys_payload)?;
    if keys.len() != 2 {
        anyhow::bail!(
            "Expected exactly 2 keys from new member {new_member_id}, found {count}",
            count = keys.len()
        );
    }
    let new_namespace_fingerprint = utils::compute_fingerprint(&keys[0]);
    let new_daml_key = keys[1].clone();
    let new_daml_fingerprint = utils::compute_fingerprint(&new_daml_key);
    tracing::info!(
        "New member namespace fingerprint: {new_namespace_fingerprint}, \
         DAML key fingerprint: {new_daml_fingerprint}"
    );

    if current_namespace_def
        .owners
        .contains(&new_namespace_fingerprint)
    {
        anyhow::bail!(
            "Namespace fingerprint {new_namespace_fingerprint} is already a DNS owner — \
             the new member appears to reuse an existing member's namespace key"
        );
    }

    let new_threshold = add_party_config.new_threshold;
    let mut new_owners = current_namespace_def.owners.clone();
    new_owners.push(new_namespace_fingerprint.clone());
    new_owners.sort();

    let new_namespace_def = DecentralizedNamespaceDefinition {
        decentralized_namespace: current_namespace_def.decentralized_namespace.clone(),
        threshold: new_threshold,
        owners: new_owners,
    };

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let party_id = &add_party_config.decentralized_party_id;
    let current_p2p = fetch_p2p_mapping(config, &synchronizer_id, party_id).await?;

    tracing::info!(
        "Current P2P mapping has {count} participant(s)",
        count = current_p2p.participants.len()
    );

    let mut new_participants = current_p2p.participants.clone();
    new_participants.push(HostingParticipant {
        participant_uid: new_member_id.clone(),
        permission: enums::ParticipantPermission::Confirmation as i32,
        // The Onboarding marker keeps the party suspended on the new member
        // until the ACS import lands and the flag-clearing round removes it.
        onboarding: Some(hosting_participant::Onboarding {}),
    });

    // Merge the new member's DAML key into the party signing keys, deduped by
    // fingerprint so a retried run can't double-add it.
    let mut signing_keys = current_p2p
        .party_signing_keys
        .map(|sk| sk.keys)
        .unwrap_or_default();
    if !signing_keys
        .iter()
        .any(|k| utils::compute_fingerprint(k) == new_daml_fingerprint)
    {
        signing_keys.push(new_daml_key);
    }

    let new_p2p = PartyToParticipant {
        party: party_id.to_string(),
        threshold: new_threshold.try_into()?,
        participants: new_participants,
        party_signing_keys: Some(SigningKeysWithThreshold {
            keys: signing_keys,
            threshold: new_threshold.try_into()?,
        }),
    };

    tracing::info!("Creating DNS add-party proposal...");
    let dns_response = authorize_with_topology_retry(
        config,
        proposal_request(
            &synchronizer_id,
            topology_mapping::Mapping::DecentralizedNamespaceDefinition(new_namespace_def.clone()),
        ),
        "add-party DNS",
    )
    .await?;
    let dns_transaction = dns_response
        .transaction
        .ok_or_else(|| anyhow::anyhow!("No DNS transaction returned"))?;

    tracing::info!("Creating P2P add-party proposal...");
    let p2p_response = authorize_with_topology_retry(
        config,
        proposal_request(
            &synchronizer_id,
            topology_mapping::Mapping::PartyToParticipant(new_p2p),
        ),
        "add-party P2P",
    )
    .await?;
    let p2p_transaction = p2p_response
        .transaction
        .ok_or_else(|| anyhow::anyhow!("No P2P transaction returned"))?;

    storage
        .write_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_DNS_PROPOSAL,
            None,
            &encode_length_prefixed_message(&dns_transaction),
        )
        .await?;
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_P2P_PROPOSAL,
            None,
            &encode_length_prefixed_message(&p2p_transaction),
        )
        .await?;
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_NEW_NAMESPACE_DEF,
            None,
            &encode_length_prefixed_message(&new_namespace_def),
        )
        .await?;

    tracing::info!("Add-party proposals created and saved successfully");
    Ok(())
}

/// Build an `AuthorizeRequest` proposing `mapping` against the synchronizer
/// store. Serial 0 lets Canton pick the next serial for the existing mapping;
/// `AllowUnvalidatedSigningKeys` is needed because the new member's keys may
/// not have reached the synchronizer store yet when the coordinator proposes
/// (same reason onboarding's proposals carry it).
pub(crate) fn proposal_request(
    synchronizer_id: &str,
    mapping: topology_mapping::Mapping,
) -> AuthorizeRequest {
    AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: enums::TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(TopologyMapping {
                    mapping: Some(mapping),
                }),
            },
        )),
        must_fully_authorize: false,
        force_changes: vec![ForceFlag::AllowUnvalidatedSigningKeys as i32],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.to_string())),
            })),
        }),
        wait_to_become_effective: None,
    }
}
