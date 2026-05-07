use bytes::{BufMut, BytesMut};
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, TopologyMapping, enums,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, BaseQuery, ListPartyToParticipantRequest, StoreId, Synchronizer,
        authorize_request, base_query, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    participant_id::CantonId,
    utils,
    workflow::{
        kick::KickConfig,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Create kick proposals
///
/// This step creates:
/// - DNS proposal to update namespace (remove kicked owner) — `KICK_DNS_PROPOSAL`
/// - P2P proposal to remove participant from mapping — `KICK_P2P_PROPOSAL`
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
    let current_p2p = get_current_p2p_mapping(config, &synchronizer_id, &party_id).await?;

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

    let new_p2p = PartyToParticipant {
        party: party_id_str.clone(),
        threshold: new_threshold.try_into()?,
        participants: new_participants,
        party_signing_keys: current_p2p.party_signing_keys,
    };

    // Create proposals using topology manager
    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    // Create DNS proposal
    tracing::info!("Creating DNS kick proposal...");
    let dns_request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: enums::TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
                        new_namespace_def.clone(),
                    )),
                }),
            },
        )),
        must_fully_authorize: false,
        force_changes: vec![],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
            })),
        }),
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
                mapping: Some(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::PartyToParticipant(new_p2p)),
                }),
            },
        )),
        must_fully_authorize: false,
        force_changes: vec![],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
            })),
        }),
        wait_to_become_effective: None,
    });

    let p2p_response = topology_client.authorize(p2p_request).await?.into_inner();
    let p2p_transaction = p2p_response
        .transaction
        .ok_or_else(|| anyhow::anyhow!("No P2P transaction returned"))?;

    // Persist proposals + supporting data to workflow storage. Each protobuf
    // is written with the same `varint(len)||proto` framing the original file
    // path used (so `read_first_message_from_bytes` works unchanged).
    let dns_bytes = encode_length_prefixed_message(&dns_transaction);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_DNS_PROPOSAL,
            None,
            &dns_bytes,
        )
        .await?;
    tracing::info!("Saved DNS kick proposal to storage");

    let p2p_bytes = encode_length_prefixed_message(&p2p_transaction);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_P2P_PROPOSAL,
            None,
            &p2p_bytes,
        )
        .await?;
    tracing::info!("Saved P2P kick proposal to storage");

    let new_namespace_bytes = encode_length_prefixed_message(&new_namespace_def);
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

/// Get the current P2P mapping for the party
async fn get_current_p2p_mapping(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result<PartyToParticipant> {
    let party_id_str = party_id.to_string();
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

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
        filter_party: party_id_str,
        filter_participant: String::new(),
    });

    let response = topology_read_client
        .list_party_to_participant(request)
        .await?
        .into_inner();

    let p2p = response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .ok_or_else(|| anyhow::anyhow!("No P2P mapping found for party {party_id}"))?;

    Ok(p2p.clone())
}

/// Encode a protobuf message as `varint(len)||proto`, matching the on-disk
/// format `utils::write_message_to_file` produces.
fn encode_length_prefixed_message<M: Message>(message: &M) -> Vec<u8> {
    let encoded = message.encode_to_vec();
    let mut buffer = BytesMut::new();
    prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
    buffer.put_slice(&encoded);
    buffer.to_vec()
}
