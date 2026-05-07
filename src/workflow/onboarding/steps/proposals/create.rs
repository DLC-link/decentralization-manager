use std::collections::HashSet;

use bytes::{BufMut, BytesMut};
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
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    participant_id::CantonId,
    utils::{self, MULTIHASH_SHA256_PREFIX},
    workflow::{
        onboarding::OnboardingConfig,
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
    },
};

/// Create topology proposals for decentralized namespace
///
/// **Important**: This step must be run by a coordinator participant that:
/// 1. Is connected to the Canton synchronizer
/// 2. Has collected keys and IDs from all peers (via Step 1)
/// 3. Has appropriate permissions to create topology proposals
///
/// This step:
/// 1. Loads all peer key payloads from `workflow_artifacts`
/// 2. Loads all participant ID payloads from `workflow_artifacts`
/// 3. Creates two topology proposals:
///    - Decentralized Namespace Definition (DNS)
///    - Party-to-Participant mapping (P2P) with embedded signing keys (Canton 3.4+)
/// 4. Saves proposals to `workflow_artifacts`
/// 5. Once the dec_party_id is known, copies every peer's
///    `PEER_PUBLIC_KEYS` + `PARTICIPANT_ID` artefact into
///    `dec_party_identity` so the rows survive the workflow run's dismissal
///    (read by `contracts::sign_submissions`, future kicks, etc.).
///
/// **Note**: If you encounter TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE errors,
/// ensure the participant is properly connected to a synchronizer first.
pub async fn create_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    onboarding_config: &OnboardingConfig,
) -> Result {
    tracing::info!("Creating topology proposals...");

    // Use party_id_prefix from onboarding config (provided via UI)
    let party_id_prefix = &onboarding_config.party_id_prefix;

    // Step 1: Load all peer key payloads from storage
    let key_payloads = storage
        .list_artifacts(instance_name, artifact_kinds::PEER_PUBLIC_KEYS)
        .await?;

    tracing::info!(
        "Found {count} peer key payloads",
        count = key_payloads.len()
    );

    if key_payloads.is_empty() {
        anyhow::bail!("No peer key payloads found for instance {instance_name}");
    }

    // Step 2: Decode each (namespace_key, daml_key) pair from its payload
    let mut namespace_keys = Vec::new();
    let mut daml_keys = Vec::new();

    for (peer_id, payload) in &key_payloads {
        tracing::info!("Loading keys from peer {peer_id}");

        let keys: Vec<SigningPublicKey> = decode_keys_payload(payload)?;

        if keys.len() != 2 {
            anyhow::bail!(
                "Expected exactly 2 keys from peer {peer_id}, but found {count}",
                count = keys.len()
            );
        }

        // First key is namespace key, second is DAML key
        namespace_keys.push(keys[0].clone());
        daml_keys.push(keys[1].clone());

        // Debug: Log fingerprints of keys being added to P2P mapping
        let daml_key_fp = utils::compute_fingerprint(&keys[1]);
        tracing::debug!("DAML key from {peer_id} has fingerprint: {daml_key_fp}");
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

    // Step 4: Load all participant ID payloads from storage
    let id_payloads = storage
        .list_artifacts(instance_name, artifact_kinds::PARTICIPANT_ID)
        .await?;

    tracing::info!(
        "Found {count} participant ID payloads",
        count = id_payloads.len()
    );

    if id_payloads.is_empty() {
        anyhow::bail!("No participant ID payloads found for instance {instance_name}");
    }

    let mut participant_ids = Vec::new();
    for (_peer_id, payload) in &id_payloads {
        let file_content = std::str::from_utf8(payload)
            .map_err(|e| anyhow::anyhow!("Invalid UTF-8 in participant id payload: {e}"))?;
        let participant_id = CantonId::parse_from_file(file_content)?;
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

    let party_id_str = format!("{party_id_prefix}::{decentralized_namespace}");
    let party_id = CantonId::parse(&party_id_str)?;
    tracing::info!("Party ID: {party_id}");

    // Persist the resolved party ID — the HTTP path reads this artefact after
    // the coordinator workflow finishes so it can return the new party id to
    // the UI.
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::PARTY_ID,
            None,
            party_id_str.as_bytes(),
        )
        .await?;

    // Identity hook (coordinator side): with the dec_party_id now known, copy
    // each peer's PEER_PUBLIC_KEYS + PARTICIPANT_ID workflow artefacts
    // into dec_party_identity, keyed by `(party_id, peer_id, kind)`. These
    // rows survive the workflow_runs row's eventual dismissal and are read by
    // post-onboarding workflows (e.g. contracts::sign_submissions).
    for (peer_id, payload) in &key_payloads {
        storage
            .write_identity(
                &party_id,
                identity_kinds::PEER_PUBLIC_KEYS,
                peer_id,
                payload,
            )
            .await?;
    }
    for (peer_id, payload) in &id_payloads {
        storage
            .write_identity(&party_id, identity_kinds::PARTICIPANT_ID, peer_id, payload)
            .await?;
    }
    tracing::info!(
        "Persisted {count} peer identity records for {party_id}",
        count = key_payloads.len()
    );

    // Step 9: Create PartyToParticipant mapping
    // Canton 3.4: PartyToParticipant now includes signing keys (PartyToKeyMapping is deprecated)
    let p2p_mapping = PartyToParticipant {
        party: party_id_str.clone(),
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
    // The coordinator creates this proposal locally, which will later be shared with peers
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

    // Step 13: Persist proposals to storage. Each protobuf is written with the
    // same `varint(len)||proto` framing the original on-disk format used.
    let dns_bytes = encode_length_prefixed_message(&dns_transaction);
    storage
        .write_artifact(instance_name, artifact_kinds::DNS_PROTO, None, &dns_bytes)
        .await?;
    tracing::info!("Saved DNS proposal to storage");

    let p2p_bytes = encode_length_prefixed_message(&p2p_transaction);
    storage
        .write_artifact(instance_name, artifact_kinds::P2P_PROTO, None, &p2p_bytes)
        .await?;
    tracing::info!("Saved P2P proposal to storage");

    let namespace_bytes = encode_length_prefixed_message(&namespace_def);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::NAMESPACE_DEF,
            None,
            &namespace_bytes,
        )
        .await?;
    tracing::info!("Saved namespace definition to storage");

    tracing::info!("All proposals created and saved successfully");
    Ok(())
}

/// Decode a payload produced by `generate_keys::encode_keys_payload` —
/// two consecutive `varint(len)||SigningPublicKey` messages.
fn decode_keys_payload(payload: &[u8]) -> Result<Vec<SigningPublicKey>> {
    let mut cursor: &[u8] = payload;
    let mut keys = Vec::with_capacity(2);
    while !cursor.is_empty() {
        let len = prost::encoding::decode_varint(&mut cursor)? as usize;
        if cursor.len() < len {
            anyhow::bail!(
                "Truncated key payload: expected {len} bytes, only {remaining} remain",
                remaining = cursor.len()
            );
        }
        let (msg_bytes, rest) = cursor.split_at(len);
        let key = SigningPublicKey::decode(msg_bytes)?;
        keys.push(key);
        cursor = rest;
    }
    Ok(keys)
}

/// Encode a protobuf message as `varint(len)||proto` — same framing
/// `utils::write_message_to_file` produced, so existing readers like
/// `read_first_message_from_bytes` round-trip cleanly.
fn encode_length_prefixed_message<M: Message>(message: &M) -> Vec<u8> {
    let encoded = message.encode_to_vec();
    let mut buffer = BytesMut::new();
    prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
    buffer.put_slice(&encoded);
    buffer.to_vec()
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
