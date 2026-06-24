use bytes::{Buf, BufMut, BytesMut};
use canton_proto_rs::com::{
    daml::ledger::api::v2::interactive::PrepareSubmissionResponse,
    digitalasset::canton::{
        crypto::{
            admin::v30::{
                ExportKeyPairRequest, ListKeysFilters, ListMyKeysRequest,
                vault_service_client::VaultServiceClient,
            },
            v30::{Signature, SignatureFormat, SigningAlgorithmSpec, SigningPublicKey},
        },
        topology::admin::v30::{
            BaseQuery, ListPartyToKeyMappingRequest, ListPartyToParticipantRequest, StoreId,
            Synchronizer, base_query, store_id, synchronizer,
            topology_manager_read_service_client::TopologyManagerReadServiceClient,
        },
    },
};
use ed25519_dalek::{Signature as DalekSignature, Signer, SigningKey, Verifier};
use sqlx::SqlitePool;
use zeroize::Zeroizing;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::CANTON_PROTOCOL_VERSION,
    error::Result,
    utils,
    workflow::storage::{WorkflowStorage, artifact_kinds, identity_kinds},
};

/// DER OCTET STRING tag
const DER_OCTET_STRING_TAG: u8 = 0x04;

/// Expected length of Ed25519 private key in bytes (32 bytes)
const ED25519_PRIVATE_KEY_LENGTH: u8 = 0x20;

/// Sign prepared ledger submissions with DAML key
///
/// This step must be run by each peer participant to sign the prepared submissions.
/// Each peer signs with their DAML signing key.
///
/// The signed bundle is persisted as a `SUBMISSION_SIGNATURES` artefact keyed
/// by this node's participant id, byte-identical to the previous on-disk file
/// `submission-signatures-{node_id}.bin`.
///
/// # Arguments
/// * `config` - Configuration with Admin API connection details
/// * `db` - Workflow storage backend (SqlitePool implementing `WorkflowStorage`)
/// * `instance_name` - Workflow run instance name (key for `workflow_artifacts`)
/// * `dec_party_id` - Decentralized party id used to look up `peer_public_keys`
///   in the `dec_party_identity` table (this run's local DAML signing key bundle)
pub async fn sign_submissions(
    config: &NodeConfig,
    db: &SqlitePool,
    instance_name: &str,
    dec_party_id: &CantonId,
) -> Result {
    tracing::info!("Signing submissions...");

    let node_id = config.participant_id().to_string();

    // Step 1: Load the DAML public key bundle that was exported during onboarding.
    // It MUST come from `dec_party_identity` (long-lived, survives the
    // originating onboarding run's dismissal) — not from `workflow_artifacts`,
    // because by the time contracts runs the onboarding run may have been
    // dismissed/aged out.
    //
    // Backfill path: onboardings that completed before the
    // `dec_party_identity` write hook was added didn't populate that table.
    // For those parties we fall back to the original onboarding run's
    // `workflow_artifacts` row, then mirror it into `dec_party_identity` so
    // subsequent contracts runs hit the fast path.
    tracing::info!(
        "Loading DAML public key bundle for {node_id} on {dec_party_id} from identity table..."
    );
    let keys_bytes = match db
        .read_identity(dec_party_id, identity_kinds::PEER_PUBLIC_KEYS, &node_id)
        .await?
    {
        Some(bytes) => bytes,
        None => {
            tracing::warn!(
                "PEER_PUBLIC_KEYS missing in identity table for {node_id} on {dec_party_id}; \
                 attempting backfill from completed onboarding artifacts"
            );
            let from_local = backfill_peer_keys(db, dec_party_id, &node_id).await?;
            let bytes = match from_local {
                Some(b) => b,
                None => {
                    tracing::warn!(
                        "Local artifacts backfill failed; querying Canton's on-chain \
                         topology (PartyToParticipant / legacy PartyToKeyMapping) to \
                         recover this node's DAML signing key for {dec_party_id}"
                    );
                    backfill_peer_keys_from_chain(config, dec_party_id)
                        .await?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "PEER_PUBLIC_KEYS not found in identity table, completed \
                                 onboarding artifacts, OR on-chain topology \
                                 (PartyToParticipant / legacy PartyToKeyMapping) for \
                                 {node_id} on {dec_party_id} — onboarding may not have \
                                 completed yet"
                            )
                        })?
                }
            };
            // Best-effort populate identity table for future calls; a failure
            // here is non-fatal — we still have the keys we need to sign now.
            if let Err(e) = db
                .write_identity(
                    dec_party_id,
                    identity_kinds::PEER_PUBLIC_KEYS,
                    &node_id,
                    &bytes,
                )
                .await
            {
                tracing::warn!(
                    "Failed to write backfilled PEER_PUBLIC_KEYS to identity table: {e:#}"
                );
            }
            bytes
        }
    };

    // The blob is two `varint(len)||SigningPublicKey` messages, written by
    // onboarding. Decode unchanged so the bytes-on-the-wire shape stays
    // identical to the previous file-based format.
    let exported_keys: Vec<SigningPublicKey> = read_all_messages_from_bytes(&keys_bytes)?;

    if exported_keys.len() != 2 {
        anyhow::bail!(
            "Expected 2 keys in PEER_PUBLIC_KEYS for {node_id}, but found {count}",
            count = exported_keys.len()
        );
    }

    // Second key is the DAML signing key (first is namespace key)
    let signing_public_key = &exported_keys[1];

    // Compute fingerprint of the newly generated DAML key
    let key_fingerprint = utils::compute_fingerprint(signing_public_key);

    tracing::info!("Using DAML key with fingerprint: {key_fingerprint}");
    tracing::debug!("This is the key that was generated in step 1 and added to P2P mapping");

    // Verify this key exists in Canton's vault
    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;

    let keys_response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: key_fingerprint.clone(),
                name: String::new(), // Search by fingerprint, not name
                purpose: vec![],
                usage: vec![],
            }),
        }))
        .await?
        .into_inner();

    if keys_response.private_keys_metadata.is_empty() {
        anyhow::bail!(
            "DAML signing key with fingerprint {key_fingerprint} not found in Canton vault. \
             This should not happen - the key was generated in step 1."
        );
    }

    tracing::debug!(
        "Verified key exists in Canton vault (found {count} matching keys)",
        count = keys_response.private_keys_metadata.len()
    );

    // Step 3: Dynamically load all prepared submissions from storage. They were
    // written by `prepare_submissions` keyed by zero-padded ordinal so
    // `list_artifacts` returns them sorted by their original creation order.
    tracing::info!("Loading prepared submissions...");
    let submission_rows = db
        .list_artifacts(instance_name, artifact_kinds::PREPARED_SUBMISSION)
        .await?;

    if submission_rows.is_empty() {
        anyhow::bail!(
            "No PREPARED_SUBMISSION artifacts found for instance {instance_name} — \
             did PrepareSubmissions run?"
        );
    }

    // Decode the per-submission `varint(len)||proto` blobs.
    let mut prepared_submissions: Vec<PrepareSubmissionResponse> =
        Vec::with_capacity(submission_rows.len());
    for (ordinal, payload) in &submission_rows {
        let prepared_sub: PrepareSubmissionResponse =
            utils::read_first_message_from_bytes(payload)?;
        tracing::debug!("Loaded prepared submission ordinal {ordinal}");
        prepared_submissions.push(prepared_sub);
    }

    tracing::debug!(
        "Loaded {count} prepared submissions",
        count = prepared_submissions.len()
    );

    // Step 4: Export the private key
    tracing::info!("Exporting private key from Canton...");
    tracing::debug!("Key fingerprint: {key_fingerprint}");

    let mut export_response = vault_client
        .export_key_pair(tonic::Request::new(ExportKeyPairRequest {
            fingerprint: key_fingerprint.clone(),
            protocol_version: CANTON_PROTOCOL_VERSION,
            password: String::new(), // Empty: the exported key pair is not passphrase-protected.
        }))
        .await
        .map_err(|e| {
            tracing::error!("ExportKeyPair RPC failed with error: {e:?}");
            tracing::error!("Attempted fingerprint: {key_fingerprint}");
            e
        })?
        .into_inner();

    // Step 5: Extract Ed25519 private key from Canton's export response.
    // Canton returns the key in a custom format with embedded metadata.
    //
    // Move the bytes directly out of the proto struct with `std::mem::take`
    // into a zeroizing buffer — avoids a second heap copy of the secret that
    // `.clone()` would create. The proto's `key_pair` is left as an empty
    // `Vec` and dropped along with `export_response` shortly after. All
    // 32-byte candidates derived below are also held in `Zeroizing<[u8; 32]>`
    // so they self-wipe on drop.
    let exported_key_data: Zeroizing<Vec<u8>> =
        Zeroizing::new(std::mem::take(&mut export_response.key_pair));
    tracing::debug!(
        "Parsing exported key pair ({len} bytes)",
        len = exported_key_data.len()
    );

    // Strategy: Try ALL possible 32-byte sequences and test each one.
    // The correct private key should verify against the public key.
    let key_size = ED25519_PRIVATE_KEY_LENGTH as usize;
    let max_offset = exported_key_data.len().saturating_sub(key_size);

    tracing::info!("Searching for valid Ed25519 private key among {max_offset} possible positions");

    let mut candidate_keys: Vec<(usize, Zeroizing<[u8; 32]>, &str)> = Vec::new();

    // First, try DER-tagged sequences (0x04 0x20 pattern)
    for offset in 0..max_offset.saturating_sub(2) {
        if exported_key_data[offset] == DER_OCTET_STRING_TAG
            && exported_key_data[offset + 1] == ED25519_PRIVATE_KEY_LENGTH
        {
            let mut key_bytes = [0u8; 32];
            key_bytes.copy_from_slice(&exported_key_data[offset + 2..offset + 2 + key_size]);
            candidate_keys.push((offset + 2, Zeroizing::new(key_bytes), "DER-tagged"));
        }
    }
    tracing::debug!(
        "Found {count} DER-tagged candidates",
        count = candidate_keys.len()
    );

    if candidate_keys.is_empty() {
        tracing::warn!("No DER-tagged sequences found, trying all possible 32-byte sequences");

        // Try every possible 32-byte sequence in the exported data
        for offset in (0..max_offset).step_by(4) {
            let mut key_bytes = [0u8; 32];
            key_bytes.copy_from_slice(&exported_key_data[offset..offset + key_size]);
            candidate_keys.push((offset, Zeroizing::new(key_bytes), "raw"));
        }

        tracing::debug!(
            "Found {count} raw 32-byte candidates",
            count = candidate_keys.len()
        );
    }

    if candidate_keys.is_empty() {
        anyhow::bail!("Could not find any Ed25519 key candidates in exported data");
    }

    tracing::info!(
        "Found {count} candidate Ed25519 key positions to try",
        count = candidate_keys.len()
    );

    // Step 6: Try each candidate key and verify it produces the correct public key
    tracing::info!("Verifying candidates against expected public key...");

    // Get the public key bytes from Canton's metadata for verification
    // Canton stores Ed25519 public keys in DER format with this structure:
    // - Bytes 0-11: DER wrapper (SEQUENCE + algorithm OID + BIT STRING header)
    // - Bytes 12-43: Raw 32-byte Ed25519 public key
    let expected_public_key_der = &signing_public_key.public_key;

    // Extract raw Ed25519 public key from DER format
    const DER_HEADER_LENGTH: usize = 12;
    const ED25519_PUBLIC_KEY_LENGTH: usize = 32;

    if expected_public_key_der.len() < DER_HEADER_LENGTH + ED25519_PUBLIC_KEY_LENGTH {
        anyhow::bail!(
            "Expected public key is too short: {result_count} bytes (need at least {expected_count})",
            result_count = expected_public_key_der.len(),
            expected_count = DER_HEADER_LENGTH + ED25519_PUBLIC_KEY_LENGTH
        );
    }

    let expected_raw_public_key = &expected_public_key_der[DER_HEADER_LENGTH..];

    let mut verified_key_bytes: Option<Zeroizing<[u8; 32]>> = None;

    for (offset, key_bytes, source) in &candidate_keys {
        let signing_key = SigningKey::from_bytes(key_bytes);
        let derived_public_bytes = signing_key.verifying_key().to_bytes();

        // Compare raw Ed25519 public keys (32 bytes)
        if derived_public_bytes.as_slice() == expected_raw_public_key {
            tracing::info!("Found matching private key at offset {offset} ({source})");
            verified_key_bytes = Some(Zeroizing::new(**key_bytes));
            break;
        }
    }

    let key_bytes = verified_key_bytes.ok_or_else(|| {
        anyhow::anyhow!(
            "None of the {count} candidate keys produced the expected public key. \
            This indicates the private key is not in the expected format in the exported data.",
            count = candidate_keys.len()
        )
    })?;
    // Drop the remaining candidates; each Zeroizing<[u8; 32]> wipes on drop.
    drop(candidate_keys);

    tracing::info!("Successfully verified Ed25519 private key");

    // Step 7: Sign transaction hashes with verified key.
    // `SigningKey` impls `Zeroize` (via the `zeroize` feature on
    // `ed25519-dalek`) and zeros its inner secret on drop.
    tracing::info!(
        "Signing {count} transaction hashes...",
        count = prepared_submissions.len()
    );

    let signing_key = SigningKey::from_bytes(&key_bytes);
    let verifying_key = signing_key.verifying_key();
    tracing::debug!("Key fingerprint used in signatures: {key_fingerprint}");

    // Sign each prepared transaction hash and create Signature protobuf messages
    let mut signatures: Vec<Signature> = Vec::new();

    for (idx, prepared_sub) in prepared_submissions.iter().enumerate() {
        let signature_bytes = signing_key
            .sign(&prepared_sub.prepared_transaction_hash)
            .to_bytes();

        // Verify locally
        let sig = DalekSignature::from_bytes(&signature_bytes);
        if verifying_key
            .verify(&prepared_sub.prepared_transaction_hash, &sig)
            .is_ok()
        {
            tracing::info!("Signature {index} verified locally", index = idx + 1);
        } else {
            tracing::error!(
                "Signature {index} failed local verification!",
                index = idx + 1
            );
        }

        // Create Signature protobuf message
        // Ed25519 signatures use CONCAT format (r || s in little-endian)
        signatures.push(Signature {
            format: SignatureFormat::Concat as i32,
            signature: signature_bytes.to_vec(),
            signed_by: key_fingerprint.clone(),
            signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
            signature_delegation: None,
        });
    }

    tracing::debug!("Generated {count} signatures", count = signatures.len());

    // Step 8: Persist signatures bundle as `SUBMISSION_SIGNATURES` artefact.
    // The blob is the same multi-message `varint(len)||proto` framing the
    // previous on-disk `submission-signatures-{node_id}.bin` used; the
    // execute step will read it back via `read_all_messages_from_bytes`.
    let payload = encode_messages_length_prefixed(&signatures);
    tracing::info!(
        "Saving signatures to artifact key {node_id} ({len} bytes)",
        len = payload.len()
    );
    db.write_artifact(
        instance_name,
        artifact_kinds::SUBMISSION_SIGNATURES,
        Some(&node_id),
        &payload,
    )
    .await?;

    tracing::info!("Signatures saved successfully");
    Ok(())
}

/// Decode a sequence of `varint(len)||proto` messages from a byte slice. Mirrors
/// `utils::read_all_messages_from_file` but operates on in-memory data — used
/// to round-trip blobs we used to read from disk.
fn read_all_messages_from_bytes<M: prost::Message + Default>(data: &[u8]) -> Result<Vec<M>> {
    let mut cursor = data;
    let mut messages = Vec::new();
    while cursor.has_remaining() {
        let len = prost::encoding::decode_varint(&mut cursor)? as usize;
        if cursor.remaining() < len {
            anyhow::bail!(
                "Incomplete message: expected {len} bytes, but only {remaining} remaining",
                remaining = cursor.remaining()
            );
        }
        let message_bytes = &cursor[..len];
        cursor.advance(len);
        messages.push(M::decode(message_bytes)?);
    }
    Ok(messages)
}

/// Encode a slice of protobuf messages as `varint(len)||proto` × N, matching the
/// byte layout produced by `utils::write_messages_to_file`. Round-trips with
/// `utils::read_all_messages_from_file` / `read_all_messages_from_bytes`.
fn encode_messages_length_prefixed<M: prost::Message>(messages: &[M]) -> Vec<u8> {
    let mut buffer = BytesMut::new();
    for message in messages {
        let encoded = message.encode_to_vec();
        prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
        buffer.put_slice(&encoded);
    }
    buffer.to_vec()
}

/// On-chain backfill: recover the dec_party's protocol signing keys from
/// Canton's topology store, then cross-reference them against this node's
/// vault. The vault key whose fingerprint matches one of the on-chain
/// signing keys is the DAML key this node contributes to the party.
///
/// The keys live in one of two places depending on when the party was
/// onboarded: `PartyToParticipant.party_signing_keys` (Canton 3.4 — what the
/// current onboarding submits) or a separate legacy `PartyToKeyMapping`
/// transaction (Canton 3.3 — parties onboarded before the switch). Both are
/// checked, newest format first.
///
/// Returns the same `varint(len)||SigningPublicKey` × 2 byte layout that
/// `read_all_messages_from_bytes` expects. Index `[0]` is unused downstream
/// (originally the namespace key), so we duplicate the DAML key to keep the
/// shape valid; the caller only reads `[1]`.
async fn backfill_peer_keys_from_chain(
    config: &NodeConfig,
    dec_party_id: &CantonId,
) -> Result<Option<Vec<u8>>> {
    let dec_party_id_str = dec_party_id.to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let base_query = BaseQuery {
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        proposals: false,
        operation: 0,
        time_query: Some(base_query::TimeQuery::HeadState(())),
        filter_signed_key: String::new(),
        protocol_version: None,
    };

    // 1. Current format: signing keys embedded on the PartyToParticipant.
    let mut topology_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;
    let p2p_response = topology_client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(base_query.clone()),
            filter_party: dec_party_id_str.clone(),
            filter_participant: String::new(),
        }))
        .await?
        .into_inner();

    let mut signing_keys: Vec<SigningPublicKey> = p2p_response
        .results
        .into_iter()
        .find_map(|r| r.item)
        .and_then(|item| item.party_signing_keys)
        .map(|k| k.keys)
        .unwrap_or_default();

    // 2. Legacy format: parties onboarded before the embedded-keys switch
    //    registered their keys via a separate PartyToKeyMapping transaction.
    if signing_keys.is_empty() {
        tracing::warn!(
            "PartyToParticipant for {dec_party_id} carries no party_signing_keys; \
             trying the legacy PartyToKeyMapping topology mapping"
        );
        let ptk_response = topology_client
            .list_party_to_key_mapping(tonic::Request::new(ListPartyToKeyMappingRequest {
                base_query: Some(base_query),
                filter_party: dec_party_id_str.clone(),
            }))
            .await?
            .into_inner();
        signing_keys = ptk_response
            .results
            .into_iter()
            .find_map(|r| r.item)
            .map(|item| item.signing_keys)
            .unwrap_or_default();
    }

    if signing_keys.is_empty() {
        tracing::warn!(
            "No protocol signing keys found on-chain for {dec_party_id} — neither \
             PartyToParticipant.party_signing_keys nor a legacy PartyToKeyMapping"
        );
        return Ok(None);
    }

    // 3. Walk the on-chain keys and pick the one our vault recognizes — that's
    //    this node's contribution. Other entries belong to peer participants
    //    and their private halves are not in our vault.
    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;
    for key in &signing_keys {
        let fingerprint = utils::compute_fingerprint(key);
        let resp = vault_client
            .list_my_keys(tonic::Request::new(ListMyKeysRequest {
                filters: Some(ListKeysFilters {
                    fingerprint: fingerprint.clone(),
                    name: String::new(),
                    purpose: vec![],
                    usage: vec![],
                }),
            }))
            .await?
            .into_inner();
        if !resp.private_keys_metadata.is_empty() {
            tracing::info!(
                "Recovered DAML signing key {fingerprint} for {dec_party_id} from the on-chain \
                 topology state"
            );
            // Encode as [namespace_placeholder, daml_key]. Downstream only
            // reads index [1], so the placeholder content is irrelevant
            // beyond the length-prefix shape — we duplicate the daml key.
            return Ok(Some(encode_messages_length_prefixed(&[
                key.clone(),
                key.clone(),
            ])));
        }
    }

    tracing::warn!(
        "None of the {count} on-chain signing keys for {dec_party_id} are present in this \
         node's vault — this node may not be a hosting participant of {dec_party_id}",
        count = signing_keys.len()
    );
    Ok(None)
}

/// Find this node's `PEER_PUBLIC_KEYS` blob from the most recent completed
/// Onboarding (or Kick — same kind of identity payload) coordinator run for
/// the given dec_party_id, by joining `workflow_artifacts` to `workflow_runs`.
/// Used as a one-shot backfill for parties whose onboarding ran before the
/// `dec_party_identity` write hook was added.
async fn backfill_peer_keys(
    db: &SqlitePool,
    dec_party_id: &CantonId,
    node_id: &str,
) -> Result<Option<Vec<u8>>> {
    let dec_party_id_str = dec_party_id.to_string();
    let row: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT a.payload \
         FROM workflow_artifacts a \
         JOIN workflow_runs r ON a.instance_name = r.instance_name \
         WHERE r.dec_party_id = ?1 \
           AND r.kind = 'Onboarding' \
           AND r.status = 'completed' \
           AND a.artifact_kind = 'peer_public_keys' \
           AND a.peer_id = ?2 \
         ORDER BY r.updated_at DESC \
         LIMIT 1",
    )
    .bind(&dec_party_id_str)
    .bind(node_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(bytes,)| bytes))
}
