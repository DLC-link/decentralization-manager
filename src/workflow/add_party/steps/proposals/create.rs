use tokio::fs;

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeysWithThreshold, SigningPublicKey},
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, TopologyMapping, enums,
        enums::ParticipantPermission, party_to_participant::HostingParticipant, topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, BaseQuery, ListPartyToParticipantRequest, StoreId, Synchronizer,
        authorize_request, base_query, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use crate::{
    config::NodeConfig,
    consts::{
        ATTESTOR_KEYS_PREFIX, DNS_ADD_PARTY_PROTO_FILENAME, NAMESPACE_DEF_FILENAME,
        NEW_NAMESPACE_DEF_FILENAME, NEW_THRESHOLD_FILENAME, P2P_ADD_PARTY_PROTO_FILENAME,
        PARTY_ID_FILENAME,
    },
    error::Result,
    utils,
    workflow::add_party::{AddPartyConfig, AddPartyDirs},
};

/// Create add party proposals
///
/// This step creates:
/// - DNS proposal to update namespace (add new owner)
/// - P2P proposal to add new participant to mapping
pub async fn create_proposals(
    config: &NodeConfig,
    dirs: &AddPartyDirs,
    add_party_config: &AddPartyConfig,
) -> Result {
    tracing::info!("Creating add party proposals...");

    // Read current namespace definition
    let namespace_file = dirs.current_config_dir.join(NAMESPACE_DEF_FILENAME);
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

    // Read new threshold
    let threshold_file = dirs.current_config_dir.join(NEW_THRESHOLD_FILENAME);
    let new_threshold: i32 = fs::read_to_string(&threshold_file)
        .await?
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse new threshold: {e}"))?;
    tracing::info!("New threshold: {new_threshold}");

    // Read new member's keys
    let new_member_id = add_party_config.new_participant_id.to_string();
    let (new_namespace_key, new_daml_key) =
        read_new_member_keys(&dirs.keys_dir, &new_member_id).await?;

    let new_namespace_fingerprint = utils::compute_fingerprint(&new_namespace_key);
    tracing::info!("New member namespace fingerprint: {new_namespace_fingerprint}");

    // Create new owner set with new member added
    let mut new_owners = current_namespace_def.owners.clone();
    new_owners.push(new_namespace_fingerprint.clone());
    new_owners.sort(); // Keep sorted for consistency

    tracing::info!("New owners count: {count}", count = new_owners.len());

    // Create new namespace definition (keeping the same namespace hash - this is critical!)
    let new_namespace_def = DecentralizedNamespaceDefinition {
        decentralized_namespace: current_namespace_def.decentralized_namespace.clone(),
        threshold: new_threshold,
        owners: new_owners,
    };

    // Get party ID using prefix from decentralized party ID (provided via UI)
    let party_id = format!(
        "{party_id_prefix}::{namespace}",
        party_id_prefix = add_party_config.decentralized_party_id.prefix,
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

    // Create new P2P mapping with new participant added
    let mut new_participants = current_p2p.participants.clone();
    new_participants.push(HostingParticipant {
        participant_uid: add_party_config.new_participant_id.to_string(),
        permission: ParticipantPermission::Confirmation as i32,
        onboarding: None,
    });

    tracing::info!(
        "New P2P mapping will have {count} participant(s)",
        count = new_participants.len()
    );

    // Add new member's DAML key to signing keys
    let new_signing_keys = if let Some(existing_keys) = current_p2p.party_signing_keys {
        let mut keys = existing_keys.keys.clone();
        keys.push(new_daml_key);
        SigningKeysWithThreshold {
            keys,
            threshold: new_threshold.try_into()?,
        }
    } else {
        SigningKeysWithThreshold {
            keys: vec![new_daml_key],
            threshold: new_threshold.try_into()?,
        }
    };

    let new_p2p = PartyToParticipant {
        party: party_id.clone(),
        threshold: new_threshold.try_into()?,
        participants: new_participants,
        party_signing_keys: Some(new_signing_keys),
    };

    // Create proposals using topology manager
    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    // Create DNS proposal
    tracing::info!("Creating DNS add party proposal...");
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

    // Create P2P add party proposal
    tracing::info!("Creating P2P add party proposal...");
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
    fs::create_dir_all(&dirs.add_party_proposals_dir).await?;

    let dns_file = dirs
        .add_party_proposals_dir
        .join(DNS_ADD_PARTY_PROTO_FILENAME);
    tracing::info!(
        "Saving DNS add party proposal to {path}",
        path = dns_file.display()
    );
    utils::write_message_to_file(&dns_transaction, &dns_file).await?;

    let p2p_file = dirs
        .add_party_proposals_dir
        .join(P2P_ADD_PARTY_PROTO_FILENAME);
    tracing::info!(
        "Saving P2P add party proposal to {path}",
        path = p2p_file.display()
    );
    utils::write_message_to_file(&p2p_transaction, &p2p_file).await?;

    // Save new namespace definition
    let new_namespace_file = dirs
        .add_party_proposals_dir
        .join(NEW_NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Saving new namespace definition to {path}",
        path = new_namespace_file.display()
    );
    utils::write_message_to_file(&new_namespace_def, &new_namespace_file).await?;

    // Save party ID
    let party_id_file = dirs.add_party_proposals_dir.join(PARTY_ID_FILENAME);
    fs::write(&party_id_file, format!("{party_id}\n")).await?;

    tracing::info!("Add party proposals created and saved successfully");
    Ok(())
}

/// Read new member's keys from the saved file
async fn read_new_member_keys(
    keys_dir: &std::path::Path,
    new_member_id: &str,
) -> Result<(SigningPublicKey, SigningPublicKey)> {
    // Find the keys file for the new member
    let keys_files = utils::find_files_by_pattern(keys_dir, ATTESTOR_KEYS_PREFIX, ".bin").await?;

    // The file should be named attestor-public-keys-{participant_id}.bin
    let keys_file = keys_files
        .iter()
        .find(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains(new_member_id))
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            anyhow::anyhow!("No keys file found for new member {new_member_id} in {keys_dir:?}")
        })?;

    tracing::info!(
        "Reading new member keys from {path}",
        path = keys_file.display()
    );

    // Read and parse the keys file - contains [namespace_key, daml_key]
    let keys: Vec<SigningPublicKey> = utils::read_all_messages_from_file(keys_file).await?;

    if keys.len() != 2 {
        anyhow::bail!("Expected 2 keys in file, got {count}", count = keys.len());
    }

    Ok((keys[0].clone(), keys[1].clone()))
}

/// Get the current P2P mapping for the party
async fn get_current_p2p_mapping(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &str,
) -> Result<PartyToParticipant> {
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
