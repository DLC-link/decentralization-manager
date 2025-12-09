use std::collections::HashSet;

use tokio::fs;

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeysWithThreshold, SigningPublicKey},
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, TopologyMapping, enums,
        party_to_participant::HostingParticipant, topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, ForceFlag, StoreId, authorize_request, store_id,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{
        ATTESTOR_KEYS_PREFIX, DNS_PROTO_FILENAME, NAMESPACE_DEF_FILENAME, P2P_PROTO_FILENAME,
        PARTICIPANT_ID_PREFIX,
    },
    error::Result,
    participant_id::CantonId,
    utils::{self, MULTIHASH_SHA256_PREFIX},
    workflow::onboarding::OnboardingDirs,
};

/// Create topology proposals for decentralized namespace
///
/// **Important**: This step must be run by a coordinator participant that:
/// 1. Is connected to the Canton synchronizer
/// 2. Has collected keys and IDs from all attestors (via Step 1)
/// 3. Has appropriate permissions to create topology proposals
///
/// This step:
/// 1. Loads all attestor key files from keys_dir
/// 2. Loads all participant ID files from ids_dir
/// 3. Creates two topology proposals:
///    - Decentralized Namespace Definition (DNS)
///    - Party-to-Participant mapping (P2P) with embedded signing keys (Canton 3.4+)
/// 4. Saves proposals to output files
///
/// **Note**: If you encounter TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE errors,
/// ensure the participant is properly connected to a synchronizer first.
pub async fn create_proposals(
    config: &NodeConfig,
    dirs: &OnboardingDirs,
    network_config: &NetworkConfig,
) -> Result {
    tracing::info!("Creating topology proposals...");

    let party_id_prefix = &network_config.application.party_id_prefix;

    // Step 1: Load all attestor key files
    if !dirs.keys_dir.exists() {
        anyhow::bail!("keys directory not found");
    }

    let key_file_paths =
        utils::find_files_by_pattern(&dirs.keys_dir, ATTESTOR_KEYS_PREFIX, ".bin").await?;

    tracing::info!(
        "Found {count} attestor key files",
        count = key_file_paths.len()
    );

    if key_file_paths.is_empty() {
        anyhow::bail!("No attestor key files found in ./keys/");
    }

    // Step 2: Load and parse all key pairs
    let mut namespace_keys = Vec::new();
    let mut daml_keys = Vec::new();

    for key_file in &key_file_paths {
        tracing::info!("Loading keys from {path}", path = key_file.display());

        let keys: Vec<SigningPublicKey> = utils::read_all_messages_from_file(key_file).await?;

        if keys.len() != 2 {
            anyhow::bail!(
                "Expected exactly 2 keys in {path}, but found {count}",
                path = key_file.display(),
                count = keys.len()
            );
        }

        // First key is namespace key, second is DAML key
        namespace_keys.push(keys[0].clone());
        daml_keys.push(keys[1].clone());

        // Debug: Log fingerprints of keys being added to P2P mapping
        let daml_key_fp = utils::compute_fingerprint(&keys[1]);
        tracing::debug!(
            "DAML key from {path} has fingerprint: {daml_key_fp}",
            path = key_file.display()
        );
    }

    // Step 3: Extract namespaces from namespace keys
    // A namespace in Canton is the fingerprint (hash) of the public key
    let mut namespaces = HashSet::new();
    for key in &namespace_keys {
        let namespace = utils::compute_fingerprint(key);
        namespaces.insert(namespace);
    }

    tracing::info!(
        "Extracted {count} unique namespaces",
        count = namespaces.len()
    );

    // Step 4: Load all participant ID files
    if !dirs.ids_dir.exists() {
        anyhow::bail!("ids directory not found");
    }

    let id_file_paths =
        utils::find_files_by_pattern(&dirs.ids_dir, PARTICIPANT_ID_PREFIX, ".bin").await?;

    tracing::info!(
        "Found {count} participant ID files",
        count = id_file_paths.len()
    );

    if id_file_paths.is_empty() {
        anyhow::bail!("No participant ID files found in ./ids/");
    }

    let mut participant_ids = Vec::new();
    for id_file in &id_file_paths {
        let file_content = fs::read_to_string(id_file).await?;
        let participant_id = CantonId::parse_from_file(&file_content)?;
        participant_ids.push(participant_id);
    }

    // Validate that namespace owners match participant count
    if namespaces.len() != participant_ids.len() {
        anyhow::bail!(
            "Mismatch: found {namespace_count} namespace owners but {participant_count} participants. Each participant must generate exactly one namespace key.",
            namespace_count = namespaces.len(),
            participant_count = participant_ids.len()
        );
    }

    // Step 5: Calculate threshold (majority)
    let threshold = namespaces.len().div_ceil(2).max(1) as u32;
    tracing::info!(
        "Using threshold {threshold} for {count} participants",
        count = namespaces.len()
    );

    // Step 6: Compute decentralized namespace
    let decentralized_namespace = compute_decentralized_namespace(&namespaces);
    tracing::info!("Computed decentralized namespace: {decentralized_namespace}");

    // Step 7: Create DecentralizedNamespaceDefinition
    let mut owners_vec: Vec<String> = namespaces.iter().cloned().collect();
    owners_vec.sort(); // Sort for consistent ordering
    tracing::debug!("DNS owners (sorted): {:?}", owners_vec);

    let namespace_def = DecentralizedNamespaceDefinition {
        decentralized_namespace: decentralized_namespace.clone(),
        threshold: threshold as i32,
        owners: owners_vec,
    };

    // Step 8: Create Party ID
    let party_id = format!("{party_id_prefix}::{decentralized_namespace}");
    tracing::info!("Party ID: {party_id}");

    // Step 9: Create PartyToParticipant mapping
    // Canton 3.4: PartyToParticipant now includes signing keys (PartyToKeyMapping is deprecated)
    let p2p_mapping = PartyToParticipant {
        party: party_id.clone(),
        threshold,
        participants: participant_ids
            .iter()
            .map(|pid| HostingParticipant {
                participant_uid: pid.to_string(),
                permission: enums::ParticipantPermission::Confirmation as i32,
                onboarding: None,
            })
            .collect(),
        party_signing_keys: Some(SigningKeysWithThreshold {
            keys: daml_keys.clone(),
            threshold,
        }),
    };

    // Debug: Log all DAML key fingerprints being added to P2P mapping
    tracing::info!(
        "Adding {count} DAML signing keys to P2P mapping:",
        count = daml_keys.len()
    );
    for (idx, key) in daml_keys.iter().enumerate() {
        let fp = utils::compute_fingerprint(key);
        tracing::info!("  Key {index}: fingerprint={fp}", index = idx + 1);
    }

    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    // Create DNS proposal in Authorized store
    // The coordinator creates this proposal locally, which will later be shared with attestors
    tracing::info!("Creating DNS proposal...");

    let dns_request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: enums::TopologyChangeOp::AddReplace as i32,
                serial: 1,
                mapping: Some(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
                        namespace_def.clone(),
                    )),
                }),
            },
        )),
        must_fully_authorize: false,
        force_changes: vec![ForceFlag::AllowUnvalidatedSigningKeys as i32],
        signed_by: vec![], // Auto-select appropriate signing keys from Authorized store
        store: Some(StoreId {
            store: Some(store_id::Store::Authorized(store_id::Authorized {})),
        }),
        wait_to_become_effective: None,
    });

    let dns_response = topology_client.authorize(dns_request).await?.into_inner();
    let dns_transaction = dns_response
        .transaction
        .ok_or_else(|| anyhow::anyhow!("No DNS transaction returned"))?;

    // Create P2P proposal in Authorized store
    tracing::info!("Creating P2P proposal...");
    let p2p_request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: enums::TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::PartyToParticipant(p2p_mapping)),
                }),
            },
        )),
        must_fully_authorize: false,
        force_changes: vec![ForceFlag::AllowUnvalidatedSigningKeys as i32],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Authorized(store_id::Authorized {})),
        }),
        wait_to_become_effective: None,
    });

    let p2p_response = topology_client.authorize(p2p_request).await?.into_inner();
    let p2p_transaction = p2p_response
        .transaction
        .ok_or_else(|| anyhow::anyhow!("No P2P transaction returned"))?;

    // Note: Canton 3.4+ - Signing keys are now included directly in the PartyToParticipant mapping above
    // No separate PartyToKeyMapping transaction needed

    // Step 13: Save proposals to files
    fs::create_dir_all(&dirs.dns_proposals_dir).await?;
    fs::create_dir_all(&dirs.p2p_proposals_dir).await?;
    fs::create_dir_all(&dirs.dns_submission_dir).await?;

    let dns_file = dirs.dns_proposals_dir.join(DNS_PROTO_FILENAME);
    tracing::info!("Saving DNS proposal to {path}", path = dns_file.display());
    utils::write_message_to_file(&dns_transaction, &dns_file).await?;

    let p2p_file = dirs.p2p_proposals_dir.join(P2P_PROTO_FILENAME);
    tracing::info!("Saving P2P proposal to {path}", path = p2p_file.display());
    utils::write_message_to_file(&p2p_transaction, &p2p_file).await?;

    // Canton 3.4+: Signing keys now embedded in P2P proposal above (no separate transaction)

    let namespace_file = dirs.dns_submission_dir.join(NAMESPACE_DEF_FILENAME);
    tracing::info!(
        "Saving namespace definition to {path}",
        path = namespace_file.display()
    );
    utils::write_message_to_file(&namespace_def, &namespace_file).await?;

    tracing::info!("All proposals created and saved successfully");
    Ok(())
}

/// Compute decentralized namespace from individual namespaces
///
/// Canton uses domain-separated hashing with HashPurpose.DecentralizedNamespaceNamespace = 37.
/// Following Canton's HashBuilder protocol, each namespace string is added with a length prefix
/// to prevent hash collisions from concatenation.
///
/// The algorithm:
/// 1. Hash the purpose ID (37) as 4-byte big-endian
/// 2. For each sorted namespace (as hex string):
///    - Hash the length of the UTF-8 encoded string as 4-byte big-endian
///    - Hash the UTF-8 bytes of the string itself
///
/// The decentralized namespace is returned in multihash format with "1220" prefix
fn compute_decentralized_namespace(namespaces: &HashSet<String>) -> String {
    use sha2::{Digest, Sha256};

    // HashPurpose.DecentralizedNamespaceNamespace = 37
    const PURPOSE_DECENTRALIZED_NAMESPACE: i32 = 37;

    let mut hasher = Sha256::new();

    // Add purpose ID as 4-byte big-endian integer (domain separation)
    hasher.update(PURPOSE_DECENTRALIZED_NAMESPACE.to_be_bytes());

    // Sort namespaces for deterministic hashing (lexicographic string order)
    let mut sorted_namespaces: Vec<_> = namespaces.iter().collect();
    sorted_namespaces.sort();

    for namespace in sorted_namespaces {
        // Convert namespace string to UTF-8 bytes
        let namespace_bytes = namespace.as_bytes();

        // Add length prefix (4-byte big-endian integer)
        let length = namespace_bytes.len() as i32;
        hasher.update(length.to_be_bytes());

        // Add the namespace string bytes
        hasher.update(namespace_bytes);
    }

    let hash_result = hasher.finalize();

    // Return multihash format: prefix + hex-encoded hash
    format!(
        "{MULTIHASH_SHA256_PREFIX}{hash}",
        hash = hex::encode(hash_result)
    )
}
