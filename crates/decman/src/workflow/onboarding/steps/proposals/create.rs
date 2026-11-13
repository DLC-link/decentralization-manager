use std::collections::HashSet;

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::SigningPublicKey,
    protocol::v30::{
        DecentralizedNamespaceDefinition, enums, party_to_participant::HostingParticipant,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, ForceFlag, StoreId, authorize_request, store_id,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils::{self, MULTIHASH_SHA256_PREFIX},
    workflow::{
        onboarding::{OnboardingConfig, steps::generate_keys::own_new_daml_key},
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
        thresholds::{PartyThresholds, check_confirmation_threshold},
        topology_version::ProposalFormat,
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

        // First key is namespace key, second is Daml key
        namespace_keys.push(keys[0].clone());
        daml_keys.push(keys[1].clone());

        // Debug: Log fingerprints of keys being added to P2P mapping
        let daml_key_fp = utils::compute_fingerprint(&keys[1]);
        tracing::debug!("Daml key from {peer_id} has fingerprint: {daml_key_fp}");
    }

    // Step 3: Extract namespaces from namespace keys
    // A namespace in Canton is the fingerprint (hash) of the public key
    let mut namespaces = HashSet::new();
    for key in &namespace_keys {
        let namespace = utils::compute_fingerprint(key);
        namespaces.insert(namespace.into());
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

    // Step 5: Determine the thresholds, validated against the resolved owner
    // and host counts — the topology is the source of truth.
    let owner_count = namespaces.len();
    let thresholds = resolve_thresholds(onboarding_config, owner_count, participant_ids.len())?;
    tracing::info!(
        "Using namespace threshold {namespace} and confirmation threshold {confirmation} for \
         {owner_count} participants (configured: {configured:?}, {configured_confirmation:?})",
        namespace = thresholds.namespace,
        confirmation = thresholds.confirmation,
        configured = onboarding_config.threshold,
        configured_confirmation = onboarding_config.confirmation_threshold,
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
        threshold: thresholds.namespace,
        owners: owners_vec,
    };

    let party_id_str = format!("{party_id_prefix}::{decentralized_namespace}");
    let party_id = CantonId::parse(&party_id_str)?;
    tracing::info!("Party ID: {party_id}");

    // Keep the resolved ID on the long-lived workflow row before writing the
    // transient PARTY_ID artefact. Completion deletes workflow_artifacts, but
    // topology discovery still needs this ID to issue an exact party query.
    storage.write_run_party_id(instance_name, &party_id).await?;

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
    // Before any topology change, so a failure leaves nothing on chain.
    crate::signing::custody::file_new_party_key(config, storage, &party_id).await?;

    // Step 9: Create PartyToParticipant mapping
    // PartyToParticipant carries the signing keys (Canton 3.4+); the standalone
    // PartyToKeyMapping is formally deprecated as of Canton 3.5.
    let p2p_mapping = thresholds.party_to_participant(
        party_id_str.clone(),
        participant_ids
            .iter()
            .map(|pid| HostingParticipant {
                participant_uid: pid.to_string(),
                permission: enums::ParticipantPermission::Confirmation as i32,
                onboarding: None,
            })
            .collect(),
        daml_keys.clone(),
    )?;

    // Debug: Log all Daml key fingerprints being added to P2P mapping
    tracing::info!(
        "Adding {count} Daml signing keys to P2P mapping:",
        count = daml_keys.len()
    );
    for (idx, key) in daml_keys.iter().enumerate() {
        let fp = utils::compute_fingerprint(key);
        tracing::info!("  Key {index}: fingerprint={fp}", index = idx + 1);
    }

    let mut topology_client = TopologyManagerWriteServiceClient::new(config.admin_channel().await?);
    let format = ProposalFormat::for_synchronizer(&utils::get_synchronizer_id(config).await?);

    // Create DNS proposal in Authorized store
    // The coordinator creates this proposal locally, which will later be shared with peers
    tracing::info!("Creating DNS proposal...");

    let dns_request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(format.proposal(
            1,
            topology_mapping::Mapping::DecentralizedNamespaceDefinition(namespace_def.clone()),
        ))),
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
        r#type: Some(authorize_request::Type::Proposal(format.proposal(
            0,
            topology_mapping::Mapping::PartyToParticipant(p2p_mapping),
        ))),
        must_fully_authorize: false,
        force_changes: vec![ForceFlag::AllowUnvalidatedSigningKeys as i32],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Authorized(store_id::Authorized {})),
        }),
        wait_to_become_effective: None,
    });

    let p2p_response = topology_client.authorize(p2p_request).await?.into_inner();
    let mut p2p_transaction = p2p_response
        .transaction
        .ok_or_else(|| anyhow::anyhow!("No P2P transaction returned"))?;
    // The vault signed with this node's Daml key if it holds it. A key DecMan
    // generated needs DecMan's own signature.
    if let Some(own_key) = own_new_daml_key(config, storage, instance_name).await? {
        crate::signing::custody::sign_with_generated_key(
            config,
            storage,
            &mut p2p_transaction,
            &own_key,
        )
        .await?;
    }

    // Signing keys ride in the PartyToParticipant mapping above (Canton 3.4+), so there is
    // no separate PartyToKeyMapping transaction — that mapping is deprecated in Canton 3.5.

    // Step 13: Persist proposals to storage. Each protobuf is written with the
    // same `varint(len)||proto` framing the original on-disk format used.
    let dns_bytes = utils::encode_length_prefixed_message(&dns_transaction);
    storage
        .write_artifact(instance_name, artifact_kinds::DNS_PROTO, None, &dns_bytes)
        .await?;
    tracing::info!("Saved DNS proposal to storage");

    let p2p_bytes = utils::encode_length_prefixed_message(&p2p_transaction);
    storage
        .write_artifact(instance_name, artifact_kinds::P2P_PROTO, None, &p2p_bytes)
        .await?;
    tracing::info!("Saved P2P proposal to storage");

    let namespace_bytes = utils::encode_length_prefixed_message(&namespace_def);
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

/// The operator-configured namespace threshold when set, otherwise the
/// default majority algorithm. The frontend pre-computes this same default, so
/// an untouched field arrives here carrying the majority value. The
/// confirmation threshold takes the namespace threshold unless configured.
fn resolve_thresholds(
    onboarding_config: &OnboardingConfig,
    owner_count: usize,
    host_count: usize,
) -> Result<PartyThresholds> {
    let namespace = match onboarding_config.threshold {
        Some(t) if (1..=owner_count as i32).contains(&t) => t,
        Some(t) => {
            anyhow::bail!("configured threshold {t} out of range 1..={owner_count} (owner count)",)
        }
        None => owner_count.div_ceil(2).max(1) as i32,
    };
    let thresholds = PartyThresholds::new(namespace, onboarding_config.confirmation_threshold);
    check_confirmation_threshold(thresholds.confirmation, host_count)?;
    Ok(thresholds)
}

/// Decode a payload produced by `generate_keys::encode_keys_payload` —
/// two consecutive `varint(len)||SigningPublicKey` messages.
///
/// `pub(crate)` so the add-party workflow decodes the new member's key
/// upload with the exact inverse of the encoder it shares with onboarding.
pub(crate) fn decode_keys_payload(payload: &[u8]) -> Result<Vec<SigningPublicKey>> {
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
pub(crate) fn compute_decentralized_namespace(namespaces: &HashSet<String>) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(threshold: Option<i32>, confirmation: Option<i32>) -> OnboardingConfig {
        OnboardingConfig::new(
            "xyz".to_string(),
            "xyz-creation".to_string(),
            threshold,
            confirmation,
        )
    }

    /// A request from decman-cli, or one with the UI field untouched, carries
    /// no confirmation threshold, so both mappings get the same value.
    #[test]
    fn an_omitted_confirmation_threshold_matches_the_namespace_one() -> Result {
        assert_eq!(
            resolve_thresholds(&config(Some(3), None), 4, 4)?,
            PartyThresholds {
                namespace: 3,
                confirmation: 3
            }
        );
        assert_eq!(
            resolve_thresholds(&config(None, None), 4, 4)?,
            PartyThresholds {
                namespace: 2,
                confirmation: 2
            }
        );
        Ok(())
    }

    #[test]
    fn a_configured_confirmation_threshold_is_used_as_given() -> Result {
        assert_eq!(
            resolve_thresholds(&config(Some(1), Some(3)), 3, 3)?,
            PartyThresholds {
                namespace: 1,
                confirmation: 3
            }
        );
        // The majority default still applies to the namespace threshold.
        assert_eq!(
            resolve_thresholds(&config(None, Some(1)), 3, 3)?,
            PartyThresholds {
                namespace: 2,
                confirmation: 1
            }
        );
        Ok(())
    }

    #[test]
    fn a_confirmation_threshold_above_the_hosts_is_refused() {
        assert!(resolve_thresholds(&config(Some(1), Some(4)), 3, 3).is_err());
        assert!(resolve_thresholds(&config(Some(1), Some(0)), 3, 3).is_err());
    }
}

/// `create_proposals` on a coordinator whose Daml key DecMan generated,
/// against a fake Canton.
#[cfg(test)]
mod with_a_generated_key {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::admin::v30::vault_service_client::VaultServiceClient,
        protocol::v30::SignedTopologyTransaction,
    };

    use super::*;
    use crate::{
        db::MIGRATOR,
        signing::{
            custody::{SoftwareKeys, new_daml_key},
            fake_admin_api::{FakeAdminApi, TestNode, VaultKey},
            key_store::{KeyStore, topology_transaction_hash},
            vault_export::ed25519_public_key,
        },
        workflow::onboarding::steps::generate_keys::encode_keys_payload,
    };

    const RUN: &str = "party-creation";

    fn onboarding() -> OnboardingConfig {
        OnboardingConfig::new("party".to_string(), RUN.to_string(), None, None)
    }

    /// The other member, whose keys came in over the network.
    fn peer() -> Result<CantonId> {
        CantonId::parse(
            "node2::1220dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        )
    }

    /// Record a coordinator run on `node`, with this node's generated Daml
    /// key and a peer's keys, as GenerateKeys and the peers' uploads leave
    /// them. Returns this node's Daml key.
    async fn collected_keys(
        fake: &FakeAdminApi,
        node: &TestNode,
        db: &SqlitePool,
    ) -> Result<SigningPublicKey> {
        sqlx::query(
            "INSERT INTO workflow_runs (
                 instance_name, kind, role, status, current_step, step_index, step_total,
                 config_json, expected_peers_json, completed_peers_json,
                 created_at, updated_at
             ) VALUES (?1, 'Onboarding', 'Coordinator', 'inprogress', 'CreateProposals',
                       1, 2, '{}', '[]', '[]', 0, 0)",
        )
        .bind(RUN)
        .execute(db)
        .await?;
        let mut vault = VaultServiceClient::new(node.config.admin_channel().await?);
        let key = new_daml_key(
            &node.config,
            db,
            &mut vault,
            "party-daml-transactions-run-1",
            None,
        )
        .await?
        .public_key;
        assert!(fake.generated().is_empty(), "the vault made no Daml key");
        let self_id = node.config.participant_id().clone();
        let members = [
            (
                self_id.clone(),
                encode_keys_payload(&VaultKey::software("party-namespace", 1).public_key, &key),
            ),
            (
                peer()?,
                encode_keys_payload(
                    &VaultKey::software("peer-namespace", 2).public_key,
                    &VaultKey::software("peer-daml", 3).public_key,
                ),
            ),
        ];
        for (member, keys) in members {
            let member_id = member.to_string();
            db.write_artifact(
                RUN,
                artifact_kinds::PEER_PUBLIC_KEYS,
                Some(&member_id),
                &keys,
            )
            .await?;
            db.write_artifact(
                RUN,
                artifact_kinds::PARTICIPANT_ID,
                Some(&member_id),
                member.to_file_format().as_bytes(),
            )
            .await?;
        }
        Ok(key)
    }

    /// Canton requires the new key to sign the mapping that adds it, and the
    /// vault cannot. So the coordinator signs the transaction's hash itself.
    /// It also files the key under the new party before any topology change.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_coordinator_signs_the_party_mapping_with_its_generated_key(
        db: SqlitePool,
    ) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::software("party-namespace", 1)]).await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;
        let key = collected_keys(&fake, &node, &db).await?;

        create_proposals(&node.config, &db, RUN, &onboarding()).await?;

        assert_eq!(fake.authorized().len(), 2, "one DNS and one P2P proposal");
        let p2p_bytes = db
            .read_artifact(RUN, artifact_kinds::P2P_PROTO, None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no P2P proposal was saved"))?;
        let p2p: SignedTopologyTransaction = utils::read_first_message_from_bytes(&p2p_bytes)?;
        let fingerprint = utils::compute_fingerprint(&key);
        let [signature] = p2p.signatures.as_slice() else {
            anyhow::bail!("expected DecMan's one signature, got {:?}", p2p.signatures);
        };
        assert_eq!(signature.signed_by, fingerprint.to_string());
        let verifying = ed25519_dalek::VerifyingKey::from_bytes(&ed25519_public_key(&key)?)?;
        let bytes: [u8; 64] = signature.signature.as_slice().try_into()?;
        verifying.verify_strict(
            &topology_transaction_hash(&p2p.transaction),
            &ed25519_dalek::Signature::from_bytes(&bytes),
        )?;
        let dns_bytes = db
            .read_artifact(RUN, artifact_kinds::DNS_PROTO, None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no DNS proposal was saved"))?;
        let dns: SignedTopologyTransaction = utils::read_first_message_from_bytes(&dns_bytes)?;
        assert!(dns.signatures.is_empty(), "the Daml key signs only the P2P");

        let party = String::from_utf8(
            db.read_artifact(RUN, artifact_kinds::PARTY_ID, None)
                .await?
                .ok_or_else(|| anyhow::anyhow!("no party id was saved"))?,
        )?;
        let store = KeyStore::open(&node.config, db.clone())
            .await?
            .ok_or_else(|| anyhow::anyhow!("the key store is off"))?;
        assert!(
            store
                .get(&CantonId::parse(&party)?, &fingerprint)
                .await?
                .is_some(),
            "the key is filed under the new party, so deploys sign with it"
        );
        Ok(())
    }

    /// A coordinator that cannot file its key stops before it proposes
    /// anything. The party would otherwise carry a key DecMan cannot sign
    /// its deploys with.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_coordinator_stops_before_any_proposal_when_it_cannot_file_its_key(
        db: SqlitePool,
    ) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::software("party-namespace", 1)]).await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;
        collected_keys(&fake, &node, &db).await?;
        // The operator replaced the key-store key after GenerateKeys ran.
        let other_key = tempfile::NamedTempFile::new()?;
        tokio::fs::write(other_key.path(), hex::encode([0x6b; 32])).await?;
        let mut config = node.config.clone();
        config.key_store_key_file = Some(other_key.path().to_path_buf());

        let error = match create_proposals(&config, &db, RUN, &onboarding()).await {
            Ok(()) => anyhow::bail!("the coordinator proposed a party it cannot sign for"),
            Err(e) => format!("{e:#}"),
        };

        assert!(error.contains("does not decrypt"), "{error}");
        assert!(fake.authorized().is_empty(), "nothing reached the topology");
        assert!(
            db.read_artifact(RUN, artifact_kinds::P2P_PROTO, None)
                .await?
                .is_none()
        );
        Ok(())
    }
}
