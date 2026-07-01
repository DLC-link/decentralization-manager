use bytes::{BufMut, BytesMut};
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{DecentralizedNamespaceDefinition, PartyToParticipant},
    topology::admin::v30::{
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        StoreId, Synchronizer, base_query, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        add_party::{AddPartyConfig, steps::generate_keys::current_ledger_offset},
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Coordinator step: export the party's current topology state and validate
/// the add against it.
///
/// Persists:
/// - `ADD_PARTY_NAMESPACE_DEF` — current `DecentralizedNamespaceDefinition`
/// - `ADD_PARTY_EXPORT_OFFSET` — this (source) participant's ledger offset,
///   captured BEFORE any topology change is submitted so `ExportPartyAcs`
///   can find the party's activation on the new member after it
///
/// Validates:
/// - the namespace and P2P mapping exist on the synchronizer
/// - the new participant is not already in the mapping
/// - `1 <= new_threshold <= current_owners + 1`
pub async fn export_state(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
    ledger_token: Option<&str>,
) -> Result {
    tracing::info!("Exporting current decentralized namespace state for add-party...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let namespace_hex = add_party_config.decentralized_party_id.namespace.to_hex();
    let namespace_def =
        fetch_namespace_definition(config, &synchronizer_id, &namespace_hex).await?;

    tracing::info!(
        "Found namespace with {count} owners, threshold {threshold}",
        count = namespace_def.owners.len(),
        threshold = namespace_def.threshold
    );

    let party_id = &add_party_config.decentralized_party_id;
    let p2p_mapping = fetch_p2p_mapping(config, &synchronizer_id, party_id).await?;

    let new_participant = &add_party_config.new_participant_id;
    if p2p_mapping
        .participants
        .iter()
        .any(|p| p.participant_uid == new_participant.to_string())
    {
        anyhow::bail!("Participant {new_participant} is already a member of {party_id}");
    }

    let post_add_owner_count = namespace_def.owners.len() as i32 + 1;
    let new_threshold = add_party_config.new_threshold;
    if new_threshold < 1 || new_threshold > post_add_owner_count {
        anyhow::bail!(
            "new_threshold must be between 1 and {post_add_owner_count} \
             (current owners + the new member); got {new_threshold}"
        );
    }

    let namespace_bytes = encode_length_prefixed_message(&namespace_def);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_NAMESPACE_DEF,
            None,
            &namespace_bytes,
        )
        .await?;
    tracing::info!("Saved namespace definition to storage");

    // Capture the export offset exactly once: a resumed run that re-enters
    // ExportState after the topology already activated must NOT move the
    // offset forward past the activation, or ExportPartyAcs won't find it.
    let existing = storage
        .read_artifact(instance_name, artifact_kinds::ADD_PARTY_EXPORT_OFFSET, None)
        .await?;
    if existing.is_none() {
        let offset = current_ledger_offset(config, ledger_token).await?;
        storage
            .write_artifact(
                instance_name,
                artifact_kinds::ADD_PARTY_EXPORT_OFFSET,
                None,
                offset.to_string().as_bytes(),
            )
            .await?;
        tracing::info!("Captured pre-activation export offset {offset}");
    }

    tracing::info!("Add-party state exported successfully");
    Ok(())
}

/// Fetch the current `DecentralizedNamespaceDefinition` from the synchronizer
/// head state.
pub(crate) async fn fetch_namespace_definition(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace_hex: &str,
) -> Result<DecentralizedNamespaceDefinition> {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(ListDecentralizedNamespaceDefinitionRequest {
        base_query: Some(head_state_query(synchronizer_id)),
        filter_namespace: namespace_hex.to_string(),
    });

    let response = topology_read_client
        .list_decentralized_namespace_definition(request)
        .await?
        .into_inner();

    response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Namespace {namespace_hex} not found in topology"))
}

/// Fetch the party's current `PartyToParticipant` mapping from the
/// synchronizer head state.
pub(crate) async fn fetch_p2p_mapping(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result<PartyToParticipant> {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(ListPartyToParticipantRequest {
        base_query: Some(head_state_query(synchronizer_id)),
        filter_party: party_id.to_string(),
        filter_participant: String::new(),
    });

    let response = topology_read_client
        .list_party_to_participant(request)
        .await?
        .into_inner();

    response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No P2P mapping found for party {party_id}"))
}

/// A head-state `BaseQuery` against the synchronizer store — the boilerplate
/// every topology read in this workflow shares.
pub(crate) fn head_state_query(synchronizer_id: &str) -> BaseQuery {
    BaseQuery {
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
    }
}

/// Encode a protobuf message as `varint(len)||proto`, the storage framing
/// every artefact reader in this codebase expects.
pub(crate) fn encode_length_prefixed_message<M: Message>(message: &M) -> Vec<u8> {
    let encoded = message.encode_to_vec();
    let mut buffer = BytesMut::new();
    prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
    buffer.put_slice(&encoded);
    buffer.to_vec()
}
