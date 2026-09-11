use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::SigningKeysWithThreshold,
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, TopologyMapping, enums,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, authorize_request,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        kick::KickConfig,
        signing_keys::{
            adopt_legacy_signing_keys, known_signing_keys_by_member, signing_keys_without_member,
        },
        storage::{WorkflowStorage, artifact_kinds},
        topology,
    },
};

/// Create kick proposals
///
/// This step creates:
/// - DNS proposal to update namespace (remove kicked owner) — `KICK_DNS_PROPOSAL`
/// - P2P proposal to remove participant from mapping, with the kicked
///   member's Daml key taken out of `party_signing_keys` and both thresholds
///   moved to the new one — `KICK_P2P_PROPOSAL`
/// - New namespace definition — `KICK_NEW_NAMESPACE_DEF` (used by submit)
/// - Full party id — `KICK_PARTY_ID` (used by submit)
pub async fn create_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    kick_config: &KickConfig,
) -> Result {
    tracing::info!("Creating kick proposals...");

    // Read current namespace definition (length-prefixed protobuf, written by
    // export_state).
    let namespace_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_NAMESPACE_DEF, None)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("KICK_NAMESPACE_DEF artifact missing — did ExportState run?")
        })?;
    let current_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&namespace_bytes)?;

    tracing::info!(
        "Current namespace: {namespace}, threshold: {threshold}, owners: {owners_count}",
        namespace = current_namespace_def.decentralized_namespace,
        threshold = current_namespace_def.threshold,
        owners_count = current_namespace_def.owners.len()
    );

    // Read kick target (plaintext fingerprint).
    let kick_target_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_TARGET_NAMESPACE, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_TARGET_NAMESPACE artifact missing"))?;
    let kick_target = String::from_utf8(kick_target_bytes)?.trim().to_string();
    tracing::info!("Kick target: {kick_target}");

    // Read new threshold (plaintext integer).
    let threshold_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_NEW_THRESHOLD, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_NEW_THRESHOLD artifact missing"))?;
    let new_threshold: i32 = String::from_utf8(threshold_bytes)?
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse new threshold: {e}"))?;
    tracing::info!("New threshold: {new_threshold}");

    // Create new owner set without kicked member
    let new_owners: Vec<String> = current_namespace_def
        .owners
        .iter()
        .filter(|owner| *owner != &kick_target)
        .cloned()
        .collect();

    if new_owners.is_empty() {
        anyhow::bail!("Cannot remove all owners from decentralized namespace");
    }

    tracing::info!("New owners count: {count}", count = new_owners.len());

    // Create new namespace definition (keeping the same namespace hash)
    let new_namespace_def = DecentralizedNamespaceDefinition {
        decentralized_namespace: current_namespace_def.decentralized_namespace.clone(),
        threshold: new_threshold,
        owners: new_owners,
    };

    // Get party ID using prefix from decentralized party ID (provided via UI)
    let party_id_str = format!(
        "{party_id_prefix}::{namespace}",
        party_id_prefix = kick_config.decentralized_party_id.prefix,
        namespace = current_namespace_def.decentralized_namespace
    );
    let party_id = CantonId::parse(&party_id_str)?;
    tracing::info!("Party ID: {party_id}");

    // Read current P2P mapping to get the current state
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // Get current P2P mapping
    let current_p2p = topology::fetch_p2p_mapping(config, &synchronizer_id, &party_id).await?;

    tracing::info!(
        "Current P2P mapping has {count} participant(s)",
        count = current_p2p.participants.len()
    );

    // Create new P2P mapping without kicked participant
    let kick_participant_str = kick_config.participant_id.to_string();
    let new_participants: Vec<_> = current_p2p
        .participants
        .into_iter()
        .filter(|p| p.participant_uid != kick_participant_str)
        .collect();

    tracing::info!(
        "New P2P mapping will have {count} participant(s)",
        count = new_participants.len()
    );

    if new_participants.is_empty() {
        anyhow::bail!("Cannot remove all participants from party mapping");
    }

    // Both thresholds move together, and the kicked member's Daml key comes
    // out. Carrying `party_signing_keys` over verbatim left the removed
    // member's key counting towards the party's signing threshold and left
    // that threshold at its old value, which every peer refuses to sign (#428).
    let current_signing_keys = current_p2p
        .party_signing_keys
        .map(|sk| sk.keys)
        .unwrap_or_default();
    let survivors: Vec<String> = new_participants
        .iter()
        .map(|p| p.participant_uid.clone())
        .collect();
    let new_signing_keys = if current_signing_keys.is_empty() {
        // A party onboarded before Canton 3.4 keeps its keys in a deprecated
        // PartyToKeyMapping. Adopting the survivors' keys moves them inline
        // and leaves the departing member's behind in the same step.
        adopt_legacy_signing_keys(config, storage, &synchronizer_id, &party_id, &survivors).await?
    } else {
        let claims =
            known_signing_keys_by_member(config, storage, &party_id, &current_signing_keys).await?;
        signing_keys_without_member(
            &current_signing_keys,
            &kick_participant_str,
            &survivors,
            &claims,
        )?
    };

    if new_signing_keys.len() != new_participants.len() {
        anyhow::bail!(
            "Kick would leave the party with {keys} signing key(s) for {members} member(s). \
             Every member contributes exactly one, so the party's key set does not match \
             its membership and the peers would refuse the proposal",
            keys = new_signing_keys.len(),
            members = new_participants.len()
        );
    }

    let new_p2p = PartyToParticipant {
        party: party_id_str.clone(),
        threshold: new_threshold.try_into()?,
        participants: new_participants,
        party_signing_keys: Some(SigningKeysWithThreshold {
            keys: new_signing_keys,
            threshold: new_threshold.try_into()?,
        }),
    };

    // Create proposals using topology manager
    let mut topology_client = TopologyManagerWriteServiceClient::new(config.admin_channel().await?);

    // Create DNS proposal
    tracing::info!("Creating DNS kick proposal...");
    let dns_request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: enums::TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
                        new_namespace_def.clone(),
                    )),
                })),
            },
        )),
        must_fully_authorize: false,
        force_changes: vec![],
        signed_by: vec![],
        store: Some(topology::synchronizer_store_id(&synchronizer_id)),
        wait_to_become_effective: None,
    });

    let dns_response = topology_client.authorize(dns_request).await?.into_inner();
    let dns_transaction = dns_response
        .transaction
        .ok_or_else(|| anyhow::anyhow!("No DNS transaction returned"))?;

    // Create P2P kick proposal
    tracing::info!("Creating P2P kick proposal...");
    let p2p_request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: enums::TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::PartyToParticipant(new_p2p)),
                })),
            },
        )),
        must_fully_authorize: false,
        force_changes: vec![],
        signed_by: vec![],
        store: Some(topology::synchronizer_store_id(&synchronizer_id)),
        wait_to_become_effective: None,
    });

    let p2p_response = topology_client.authorize(p2p_request).await?.into_inner();
    let p2p_transaction = p2p_response
        .transaction
        .ok_or_else(|| anyhow::anyhow!("No P2P transaction returned"))?;

    // Persist proposals + supporting data to workflow storage. Each protobuf
    // is written with the same `varint(len)||proto` framing the original file
    // path used (so `read_first_message_from_bytes` works unchanged).
    let dns_bytes = utils::encode_length_prefixed_message(&dns_transaction);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_DNS_PROPOSAL,
            None,
            &dns_bytes,
        )
        .await?;
    tracing::info!("Saved DNS kick proposal to storage");

    let p2p_bytes = utils::encode_length_prefixed_message(&p2p_transaction);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_P2P_PROPOSAL,
            None,
            &p2p_bytes,
        )
        .await?;
    tracing::info!("Saved P2P kick proposal to storage");

    let new_namespace_bytes = utils::encode_length_prefixed_message(&new_namespace_def);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_NEW_NAMESPACE_DEF,
            None,
            &new_namespace_bytes,
        )
        .await?;
    tracing::info!("Saved new namespace definition to storage");

    // Save party ID — plaintext, mirrors the previous file write that
    // included a trailing newline (submit trims it).
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_PARTY_ID,
            None,
            format!("{party_id}\n").as_bytes(),
        )
        .await?;

    tracing::info!("Kick proposals created and saved successfully");
    Ok(())
}
