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
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        change_threshold::ChangeThresholdConfig,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Create the change-threshold proposals.
///
/// This step creates:
/// - DNS proposal re-issuing the namespace with the new threshold (same
///   owners) — `CHANGE_THRESHOLD_DNS_PROPOSAL`
/// - P2P proposal re-issuing the party mapping with the new threshold (same
///   participants) — `CHANGE_THRESHOLD_P2P_PROPOSAL`
/// - New namespace definition — `CHANGE_THRESHOLD_NEW_NAMESPACE_DEF` (used by
///   submit to poll the topology)
/// - Full party id — `CHANGE_THRESHOLD_PARTY_ID` (used by submit)
pub async fn create_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    change_config: &ChangeThresholdConfig,
) -> Result {
    tracing::info!("Creating change-threshold proposals...");

    // Read current namespace definition (length-prefixed protobuf, written by
    // export_state).
    let namespace_bytes = storage
        .read_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_NAMESPACE_DEF,
            None,
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "CHANGE_THRESHOLD_NAMESPACE_DEF artifact missing — did ExportState run?"
            )
        })?;
    let current_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&namespace_bytes)?;

    let new_threshold = change_config.new_threshold;
    tracing::info!(
        "Current namespace: {namespace}, threshold: {threshold} -> {new_threshold}, owners: {owners_count}",
        namespace = current_namespace_def.decentralized_namespace,
        threshold = current_namespace_def.threshold,
        owners_count = current_namespace_def.owners.len()
    );

    // Keep the full owner set — only the threshold changes.
    let new_namespace_def = DecentralizedNamespaceDefinition {
        decentralized_namespace: current_namespace_def.decentralized_namespace.clone(),
        threshold: new_threshold,
        owners: current_namespace_def.owners.clone(),
    };

    // Party id: prefix (from the request) :: current namespace fingerprint.
    // The namespace hash is over the owner set only, which is unchanged, so
    // the party id is stable across the threshold change.
    let party_id_str = format!(
        "{party_id_prefix}::{namespace}",
        party_id_prefix = change_config.decentralized_party_id.prefix,
        namespace = current_namespace_def.decentralized_namespace
    );
    let party_id = CantonId::parse(&party_id_str)?;
    tracing::info!("Party ID: {party_id}");

    // Read the current P2P mapping so we re-issue it verbatim with only the
    // threshold changed.
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let current_p2p = get_current_p2p_mapping(config, &synchronizer_id, &party_id).await?;
    tracing::info!(
        "Current P2P mapping has {count} participant(s), threshold {threshold}",
        count = current_p2p.participants.len(),
        threshold = current_p2p.threshold,
    );

    let new_p2p = PartyToParticipant {
        party: party_id_str.clone(),
        threshold: new_threshold.try_into()?,
        participants: current_p2p.participants,
        party_signing_keys: current_p2p.party_signing_keys,
    };

    // Create proposals using topology manager
    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    // Create DNS proposal
    tracing::info!("Creating DNS change-threshold proposal...");
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

    // Create P2P proposal
    tracing::info!("Creating P2P change-threshold proposal...");
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

    // Persist proposals + supporting data. Each protobuf is written with the
    // same `varint(len)||proto` framing the file path used, so
    // `read_first_message_from_bytes` works unchanged.
    let dns_bytes = utils::encode_length_prefixed_message(&dns_transaction);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_DNS_PROPOSAL,
            None,
            &dns_bytes,
        )
        .await?;
    tracing::info!("Saved DNS change-threshold proposal to storage");

    let p2p_bytes = utils::encode_length_prefixed_message(&p2p_transaction);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_P2P_PROPOSAL,
            None,
            &p2p_bytes,
        )
        .await?;
    tracing::info!("Saved P2P change-threshold proposal to storage");

    let new_namespace_bytes = utils::encode_length_prefixed_message(&new_namespace_def);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_NEW_NAMESPACE_DEF,
            None,
            &new_namespace_bytes,
        )
        .await?;
    tracing::info!("Saved new namespace definition to storage");

    // Save party ID — plaintext with a trailing newline (submit trims it).
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_PARTY_ID,
            None,
            format!("{party_id}\n").as_bytes(),
        )
        .await?;

    tracing::info!("Change-threshold proposals created and saved successfully");
    Ok(())
}

/// Get the current P2P mapping for the party.
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
