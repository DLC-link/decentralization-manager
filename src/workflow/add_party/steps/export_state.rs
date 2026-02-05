use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest, StoreId,
    Synchronizer, base_query, store_id, synchronizer,
    topology_manager_read_service_client::TopologyManagerReadServiceClient,
};

use crate::{
    config::NodeConfig,
    consts::{NAMESPACE_DEF_FILENAME, NEW_THRESHOLD_FILENAME},
    error::Result,
    utils,
    workflow::add_party::{AddPartyConfig, AddPartyDirs},
};

/// Export current decentralized namespace state for add party workflow
///
/// This step exports:
/// - Current namespace definition
/// - New threshold after adding member
pub async fn export_state(
    config: &NodeConfig,
    dirs: &AddPartyDirs,
    add_party_config: &AddPartyConfig,
) -> Result {
    tracing::info!("Exporting current decentralized namespace state...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let namespace_hex = add_party_config.decentralized_party_id.namespace.to_hex();
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
    tracing::debug!("Current DNS owners:");
    for (i, owner) in namespace_def.owners.iter().enumerate() {
        tracing::debug!("  Owner {i}: {owner}");
    }

    // Save namespace definition
    let namespace_file = dirs.current_config_dir.join(NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Saving namespace definition to {path}",
        path = namespace_file.display()
    );
    utils::write_message_to_file(namespace_def, &namespace_file).await?;

    // Verify P2P mapping exists and validate the new participant is not already present
    let party_id_prefix = &add_party_config.decentralized_party_id.prefix;
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

    let new_participant = &add_party_config.new_participant_id;
    tracing::info!("New participant to add: {new_participant}");

    // Verify the participant is not already in the mapping
    if p2p_mapping
        .participants
        .iter()
        .any(|p| p.participant_uid == new_participant.to_string())
    {
        anyhow::bail!("Participant {new_participant} is already in the P2P mapping");
    }

    // Use the threshold configured by the user
    let new_member_count = namespace_def.owners.len() + 1;
    let new_threshold = add_party_config.new_threshold;

    tracing::info!("New member count after add: {new_member_count}");
    tracing::info!("New threshold (configured): {new_threshold}");

    // Validate threshold
    if new_threshold < 1 {
        anyhow::bail!("New threshold must be at least 1, got {new_threshold}");
    }
    if new_threshold as usize > new_member_count {
        anyhow::bail!(
            "New threshold {new_threshold} cannot exceed new member count {new_member_count}"
        );
    }

    // Save new threshold
    let threshold_file = dirs.current_config_dir.join(NEW_THRESHOLD_FILENAME);
    tokio::fs::write(&threshold_file, format!("{new_threshold}\n"))
        .await
        .with_context(|| format!("Failed to write '{}'", threshold_file.display()))?;

    tracing::info!(
        "State exported successfully to {path}",
        path = dirs.current_config_dir.display()
    );
    Ok(())
}
