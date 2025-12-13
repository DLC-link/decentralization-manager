use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest, StoreId,
    Synchronizer, base_query, store_id, synchronizer,
    topology_manager_read_service_client::TopologyManagerReadServiceClient,
};

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{
        KICK_PARTICIPANT_ID_FILENAME, KICK_TARGET_FILENAME, NAMESPACE_DEF_FILENAME,
        NEW_THRESHOLD_FILENAME,
    },
    error::Result,
    utils,
    workflow::kick::{KickConfig, KickDirs},
};

/// Export current decentralized namespace state
///
/// This step exports:
/// - Current namespace definition
/// - Kick target (namespace fingerprint to remove)
/// - Kick participant ID
/// - New threshold after kick
pub async fn export_state(
    config: &NodeConfig,
    dirs: &KickDirs,
    network_config: &NetworkConfig,
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

    // Save namespace definition
    let namespace_file = dirs.kick_config_dir.join(NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Saving namespace definition to {path}",
        path = namespace_file.display()
    );
    utils::write_message_to_file(namespace_def, &namespace_file).await?;

    // Get P2P mapping to find participants
    let party_id_prefix = &network_config.application.party_id_prefix;
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

    // Save kick target
    let kick_target_file = dirs.kick_config_dir.join(KICK_TARGET_FILENAME);
    tokio::fs::write(&kick_target_file, format!("{kick_target_hex}\n"))
        .await
        .with_context(|| format!("Failed to write '{}'", kick_target_file.display()))?;

    // Save kick participant ID
    let kick_participant_file = dirs.kick_config_dir.join(KICK_PARTICIPANT_ID_FILENAME);
    tokio::fs::write(&kick_participant_file, format!("{kick_participant}\n"))
        .await
        .with_context(|| format!("Failed to write '{}'", kick_participant_file.display()))?;

    // Calculate new threshold (majority of remaining members)
    let remaining_members = namespace_def.owners.len() - 1;
    let new_threshold = remaining_members.div_ceil(2).max(1) as i32;

    tracing::info!("Remaining members after kick: {remaining_members}");
    tracing::info!("New threshold: {new_threshold}");

    // Save new threshold
    let threshold_file = dirs.kick_config_dir.join(NEW_THRESHOLD_FILENAME);
    tokio::fs::write(&threshold_file, format!("{new_threshold}\n"))
        .await
        .with_context(|| format!("Failed to write '{}'", threshold_file.display()))?;

    tracing::info!(
        "State exported successfully to {path}",
        path = dirs.kick_config_dir.display()
    );
    Ok(())
}
