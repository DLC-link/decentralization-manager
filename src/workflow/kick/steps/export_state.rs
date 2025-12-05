use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
    StoreId, Synchronizer, base_query, store_id, synchronizer,
    topology_manager_read_service_client::TopologyManagerReadServiceClient,
};

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::NAMESPACE_DEF_FILENAME,
    error::Result,
    utils,
    workflow::kick::{KickConfig, KickDirs},
};

/// Export current decentralized namespace state
///
/// Corresponds to: 00_ExportCurrentState.sc
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
        "Found namespace with {} owners, threshold {}",
        namespace_def.owners.len(),
        namespace_def.threshold
    );
    tracing::debug!("DNS owners:");
    for (i, owner) in namespace_def.owners.iter().enumerate() {
        tracing::debug!("  Owner {i}: {owner}");
    }

    if namespace_def.owners.len() < 2 {
        anyhow::bail!(
            "Cannot kick from namespace with only {} owner(s)",
            namespace_def.owners.len()
        );
    }

    // Save namespace definition
    let namespace_file = dirs.kick_config_dir.join(NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Saving namespace definition to {}",
        namespace_file.display()
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

    let kick_participant_str = kick_config.participant_ids[0].to_string();
    tracing::info!("Participant to kick: {kick_participant_str}");

    if !p2p_mapping
        .participants
        .iter()
        .any(|p| p.participant_uid == kick_participant_str)
    {
        anyhow::bail!("Participant {kick_participant_str} not in P2P mapping");
    }

    // Load participant-to-namespace mapping from onboarding
    let mapping_file = dirs
        .kick_config_dir
        .parent()
        .unwrap()
        .join("participant-namespace-mapping.txt");

    tracing::info!(
        "Loading participant-namespace mapping from {}",
        mapping_file.display()
    );

    let namespace_key_fp = if mapping_file.exists() {
        let mapping_content = tokio::fs::read_to_string(&mapping_file).await?;
        let mut found_namespace = None;

        for line in mapping_content.lines() {
            if let Some((participant, namespace)) = line.split_once('=') {
                if participant.trim() == kick_participant_str {
                    found_namespace = Some(namespace.trim().to_string());
                    tracing::info!(
                        "Found mapping: {kick_participant_str} -> {namespace}",
                        namespace = namespace.trim()
                    );
                    break;
                }
            }
        }

        found_namespace.ok_or_else(|| {
            anyhow::anyhow!(
                "No namespace mapping found for participant {kick_participant_str} in {mapping_file:?}"
            )
        })?
    } else {
        anyhow::bail!(
            "Participant-namespace mapping file not found: {}. \
             This file is created during onboarding. Please re-run onboarding workflow.",
            mapping_file.display()
        )
    };

    tracing::info!("Using namespace fingerprint {namespace_key_fp} for participant {kick_participant_str}");

    // Verify the namespace is in DNS owners
    if !namespace_def.owners.contains(&namespace_key_fp) {
        anyhow::bail!(
            "Namespace fingerprint {namespace_key_fp} for participant {kick_participant_str} \
             is not in DNS owners: {:?}",
            namespace_def.owners
        );
    }

    let kick_target_hex = namespace_key_fp;

    tracing::info!(
        "Successfully mapped participant {kick_participant_str} to DNS owner {kick_target_hex}"
    );

    // Save kick target
    let kick_target_file = dirs.kick_config_dir.join("kick-target");
    tokio::fs::write(&kick_target_file, format!("{kick_target_hex}\n")).await?;

    // Save kick participant ID
    let kick_participant_file = dirs.kick_config_dir.join("kick-participant-id");
    tokio::fs::write(&kick_participant_file, format!("{kick_participant_str}\n")).await?;

    // Calculate new threshold (majority of remaining members)
    let remaining_members = namespace_def.owners.len() - kick_config.participant_ids.len();
    let new_threshold = remaining_members.div_ceil(2).max(1) as i32;

    tracing::info!("Remaining members after kick: {remaining_members}");
    tracing::info!("New threshold: {new_threshold}");

    // Save new threshold
    let threshold_file = dirs.kick_config_dir.join("new-threshold");
    tokio::fs::write(&threshold_file, format!("{new_threshold}\n")).await?;

    tracing::info!(
        "State exported successfully to {}",
        dirs.kick_config_dir.display()
    );
    Ok(())
}
