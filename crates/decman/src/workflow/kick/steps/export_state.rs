use bytes::{BufMut, BytesMut};
use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest, StoreId,
    Synchronizer, base_query, store_id, synchronizer,
    topology_manager_read_service_client::TopologyManagerReadServiceClient,
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        kick::KickConfig,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Export current decentralized namespace state
///
/// This step exports:
/// - Current namespace definition (`KICK_NAMESPACE_DEF`)
/// - Kick target (`KICK_TARGET_NAMESPACE` — the namespace fingerprint to remove)
/// - Kick participant ID (`KICK_TARGET_PARTICIPANT`)
/// - New threshold after kick (`KICK_NEW_THRESHOLD`)
pub async fn export_state(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    kick_config: &KickConfig,
) -> Result {
    tracing::info!("Exporting current decentralized namespace state...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let namespace_hex = kick_config.decentralized_party_id.namespace.to_hex();
    tracing::info!("Querying namespace: {namespace_hex}");

    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(ListDecentralizedNamespaceDefinitionRequest {
        base_query: Some(BaseQuery {
            store: Some(StoreId {
                store: Some(store_id::Store::Synchronizer(Synchronizer {
                    kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
                })),
            }),
            proposals: false,
            operation: 0,
            time_query: Some(base_query::TimeQuery::HeadState(())),
            filter_signed_key: String::new(),
            protocol_version: None,
        }),
        filter_namespace: namespace_hex.clone(),
    });

    let response = topology_read_client
        .list_decentralized_namespace_definition(request)
        .await?
        .into_inner();

    if response.results.is_empty() {
        anyhow::bail!("Namespace {namespace_hex} not found in topology");
    }

    let namespace_def = response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .ok_or_else(|| anyhow::anyhow!("Namespace definition missing from response"))?;

    tracing::info!(
        "Found namespace with {count} owners, threshold {threshold}",
        count = namespace_def.owners.len(),
        threshold = namespace_def.threshold
    );
    tracing::debug!("DNS owners:");
    for (i, owner) in namespace_def.owners.iter().enumerate() {
        tracing::debug!("  Owner {i}: {owner}");
    }

    if namespace_def.owners.len() < 2 {
        anyhow::bail!(
            "Cannot kick from namespace with only {count} owner(s)",
            count = namespace_def.owners.len()
        );
    }

    // Save namespace definition as a length-prefixed protobuf — same byte
    // shape as the file written by `utils::write_message_to_file`.
    let namespace_bytes = encode_length_prefixed_message(namespace_def);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_NAMESPACE_DEF,
            None,
            &namespace_bytes,
        )
        .await?;
    tracing::info!("Saved namespace definition to storage");

    // Get P2P mapping to find participants
    // Use the prefix from the decentralized party ID provided via UI
    let party_id_prefix = &kick_config.decentralized_party_id.prefix;
    let party_id = format!("{party_id_prefix}::{namespace_hex}");
    tracing::info!("Querying P2P mapping for party: {party_id}");

    let p2p_request = tonic::Request::new(ListPartyToParticipantRequest {
        base_query: Some(BaseQuery {
            store: Some(StoreId {
                store: Some(store_id::Store::Synchronizer(Synchronizer {
                    kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
                })),
            }),
            proposals: false,
            operation: 0,
            time_query: Some(base_query::TimeQuery::HeadState(())),
            filter_signed_key: String::new(),
            protocol_version: None,
        }),
        filter_party: party_id.clone(),
        filter_participant: String::new(),
    });

    let p2p_response = topology_read_client
        .list_party_to_participant(p2p_request)
        .await?
        .into_inner();

    let p2p_mapping = p2p_response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .ok_or_else(|| anyhow::anyhow!("No P2P mapping found for party {party_id}"))?;

    let kick_participant = &kick_config.participant_id;
    tracing::info!("Participant to kick: {kick_participant}");

    if !p2p_mapping
        .participants
        .iter()
        .any(|p| p.participant_uid == kick_participant.to_string())
    {
        anyhow::bail!("Participant {kick_participant} not in P2P mapping");
    }

    // Use the namespace fingerprint provided as parameter
    let kick_target_hex = &kick_config.namespace_fingerprint;

    tracing::info!(
        "Using provided namespace fingerprint for participant {kick_participant}: {kick_target_hex}"
    );

    // Verify that the namespace fingerprint is in the DNS owners list
    if !namespace_def.owners.contains(kick_target_hex) {
        anyhow::bail!(
            "Namespace fingerprint {kick_target_hex} is not in the DNS owners list. Available owners: {:?}",
            namespace_def.owners
        );
    }

    tracing::info!(
        "Successfully mapped participant {kick_participant} to DNS owner {kick_target_hex}"
    );

    // Save kick target — plaintext, mirrors the previous file write that
    // included a trailing newline so existing reader trim behaviour matches.
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_TARGET_NAMESPACE,
            None,
            format!("{kick_target_hex}\n").as_bytes(),
        )
        .await?;

    // Save kick participant ID — plaintext.
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_TARGET_PARTICIPANT,
            None,
            format!("{kick_participant}\n").as_bytes(),
        )
        .await?;

    // Use the threshold configured by the user
    let remaining_members = namespace_def.owners.len() - 1;
    let new_threshold = kick_config.new_threshold;

    tracing::info!("Remaining members after kick: {remaining_members}");
    tracing::info!("New threshold (configured): {new_threshold}");

    // Save new threshold — plaintext.
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_NEW_THRESHOLD,
            None,
            format!("{new_threshold}\n").as_bytes(),
        )
        .await?;

    tracing::info!("State exported successfully to workflow storage");
    Ok(())
}

/// Encode a single protobuf message with a varint length prefix, matching the
/// byte layout produced by `utils::write_message_to_file`. Used so reads via
/// `utils::read_first_message_from_bytes` keep working unchanged.
fn encode_length_prefixed_message<M: Message>(message: &M) -> Vec<u8> {
    let encoded = message.encode_to_vec();
    let mut buffer = BytesMut::new();
    prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
    buffer.put_slice(&encoded);
    buffer.to_vec()
}
