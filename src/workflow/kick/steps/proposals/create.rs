use tokio::fs;

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, TopologyMapping, enums,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, StoreId, Synchronizer, authorize_request, store_id, synchronizer,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use crate::{
    config::NodeConfig,
    consts::{
        DNS_KICK_PROTO_FILENAME, KICK_TARGET_FILENAME, NAMESPACE_DEF_FILENAME,
        NEW_NAMESPACE_DEF_FILENAME, NEW_THRESHOLD_FILENAME, P2P_KICK_PROTO_FILENAME,
        PARTY_ID_FILENAME,
    },
    error::Result,
    utils,
    workflow::kick::{KickConfig, KickDirs},
};

/// Create kick proposals
///
/// This step creates:
/// - DNS proposal to update namespace (remove kicked owner)
/// - P2P proposal to remove participant from mapping
pub async fn create_proposals(
    config: &NodeConfig,
    dirs: &KickDirs,
    kick_config: &KickConfig,
) -> Result {
    tracing::info!("Creating kick proposals...");

    // Read current namespace definition
    let namespace_file = dirs.kick_config_dir.join(NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Reading current namespace definition from {path}",
        path = namespace_file.display()
    );
    let current_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&namespace_file).await?;

    tracing::info!(
        "Current namespace: {namespace}, threshold: {threshold}, owners: {owners_count}",
        namespace = current_namespace_def.decentralized_namespace,
        threshold = current_namespace_def.threshold,
        owners_count = current_namespace_def.owners.len()
    );

    // Read kick target
    let kick_target_file = dirs.kick_config_dir.join(KICK_TARGET_FILENAME);
    let kick_target = fs::read_to_string(&kick_target_file)
        .await?
        .trim()
        .to_string();
    tracing::info!("Kick target: {kick_target}");

    // Read new threshold
    let threshold_file = dirs.kick_config_dir.join(NEW_THRESHOLD_FILENAME);
    let new_threshold: i32 = fs::read_to_string(&threshold_file)
        .await?
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
    let party_id = format!(
        "{party_id_prefix}::{namespace}",
        party_id_prefix = kick_config.decentralized_party_id.prefix,
        namespace = current_namespace_def.decentralized_namespace
    );
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
        party: party_id.clone(),
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

    // Save proposals to files
    fs::create_dir_all(&dirs.kick_proposals_dir).await?;

    let dns_file = dirs.kick_proposals_dir.join(DNS_KICK_PROTO_FILENAME);
    tracing::info!(
        "Saving DNS kick proposal to {path}",
        path = dns_file.display()
    );
    utils::write_message_to_file(&dns_transaction, &dns_file).await?;

    let p2p_file = dirs.kick_proposals_dir.join(P2P_KICK_PROTO_FILENAME);
    tracing::info!(
        "Saving P2P kick proposal to {path}",
        path = p2p_file.display()
    );
    utils::write_message_to_file(&p2p_transaction, &p2p_file).await?;

    // Save new namespace definition
    let new_namespace_file = dirs.kick_proposals_dir.join(NEW_NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Saving new namespace definition to {path}",
        path = new_namespace_file.display()
    );
    utils::write_message_to_file(&new_namespace_def, &new_namespace_file).await?;

    // Save party ID
    let party_id_file = dirs.kick_proposals_dir.join(PARTY_ID_FILENAME);
    fs::write(&party_id_file, format!("{party_id}\n")).await?;

    tracing::info!("Kick proposals created and saved successfully");
    Ok(())
}

/// Get the current P2P mapping for the party
async fn get_current_p2p_mapping(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &str,
) -> Result<PartyToParticipant> {
    use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
        BaseQuery, ListPartyToParticipantRequest, StoreId, Synchronizer, base_query, store_id,
        synchronizer, topology_manager_read_service_client::TopologyManagerReadServiceClient,
    };

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
        filter_party: party_id.to_string(),
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
