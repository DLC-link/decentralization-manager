use bytes::{Buf, BufMut, BytesMut};
use canton_proto_rs::com::{
    daml::ledger::api::v2::interactive::PrepareSubmissionResponse,
    digitalasset::canton::{
        crypto::{
            admin::v30::{
                ListKeysFilters, ListMyKeysRequest, vault_service_client::VaultServiceClient,
            },
            v30::SigningPublicKey,
        },
        topology::admin::v30::{
            BaseQuery, ListPartyToKeyMappingRequest, ListPartyToParticipantRequest, StoreId,
            Synchronizer, base_query,
            list_party_to_key_mapping_response::result::Item as PartyToKeyItem,
            list_party_to_participant_response::result::Item as P2pItem, store_id, synchronizer,
            topology_manager_read_service_client::TopologyManagerReadServiceClient,
        },
    },
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    signing::{PreparedTransactionHash, SigningKeyContext, select_signer},
    utils,
    workflow::storage::{WorkflowStorage, artifact_kinds, identity_kinds},
};

/// Sign prepared ledger submissions with Daml key
///
/// This step must be run by each peer participant to sign the prepared submissions.
/// Each peer signs with their Daml signing key.
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
///   in the `dec_party_identity` table (this run's local Daml signing key bundle)
pub async fn sign_submissions(
    config: &NodeConfig,
    db: &SqlitePool,
    instance_name: &str,
    dec_party_id: &CantonId,
) -> Result {
    tracing::info!("Signing submissions...");

    let node_id = config.participant_id().to_string();

    // Step 1: Load the Daml public key bundle that was exported during onboarding.
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
        "Loading Daml public key bundle for {node_id} on {dec_party_id} from identity table..."
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
                         recover this node's Daml signing key for {dec_party_id}"
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

    // Second key is the Daml signing key (first is namespace key)
    let signing_public_key = &exported_keys[1];

    // Compute fingerprint of the newly generated Daml key
    let key_fingerprint = utils::compute_fingerprint(signing_public_key);

    tracing::info!("Using Daml key with fingerprint: {key_fingerprint}");
    tracing::debug!("This is the key that was generated in step 1 and added to P2P mapping");

    // Verify this key exists in Canton's vault. Keep the channel: the signing
    // backend reuses it instead of opening a second connection.
    let admin_channel = config.admin_channel().await?;
    let mut vault_client = VaultServiceClient::new(admin_channel.clone());

    let keys_response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: key_fingerprint.clone(),
                name: String::new(), // Search by fingerprint, not name
                purpose: vec![],
                usage_v30: vec![],
            }),
            base_request: None,
        }))
        .await?
        .into_inner();

    if keys_response.private_keys_metadata.is_empty() {
        anyhow::bail!(
            "Daml signing key with fingerprint {key_fingerprint} not found in Canton vault. \
             This should not happen - the key was generated in step 1."
        );
    }

    tracing::debug!(
        "Verified key exists in Canton vault (found {count} matching keys)",
        count = keys_response.private_keys_metadata.len()
    );

    // Capture the underlying KMS key id, present only for KMS-backed keys. The
    // signer selection uses it to route a non-exportable key to a KMS backend.
    let kms_key_id = keys_response
        .private_keys_metadata
        .first()
        .and_then(|m| m.kms_key_id.clone());

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

    // Step 4: Sign each prepared-transaction hash with the party's Daml key via
    // the selected signing backend. The backend abstracts *how* the key signs —
    // exporting it and signing locally with Ed25519 today (JCE keys), or asking
    // a KMS to sign a non-exportable key (follow-up). Everything else in this
    // step is provider-independent.
    let hashes: Vec<PreparedTransactionHash> = prepared_submissions
        .into_iter()
        .map(|s| PreparedTransactionHash::new(s.prepared_transaction_hash))
        .collect();

    let key_context = SigningKeyContext {
        fingerprint: key_fingerprint.clone(),
        public_key: signing_public_key.clone(),
        kms_key_id,
    };

    let signer = select_signer(&key_context, admin_channel).await?;
    let signatures = signer.sign(&hashes, &key_context).await?;

    // Step 5: Persist signatures bundle as `SUBMISSION_SIGNATURES` artefact.
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
/// signing keys is the Daml key this node contributes to the party.
///
/// The keys live in one of two places depending on when the party was
/// onboarded: `PartyToParticipant.party_signing_keys` (Canton 3.4 — what the
/// current onboarding submits) or a separate legacy `PartyToKeyMapping`
/// transaction (Canton 3.3 — parties onboarded before the switch). Both are
/// checked, newest format first.
///
/// Returns the same `varint(len)||SigningPublicKey` × 2 byte layout that
/// `read_all_messages_from_bytes` expects. Index `[0]` is unused downstream
/// (originally the namespace key), so we duplicate the Daml key to keep the
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
        client_version: None,
    };

    // 1. Current format: signing keys embedded on the PartyToParticipant.
    let mut topology_client = TopologyManagerReadServiceClient::new(config.admin_channel().await?);
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
        .find_map(|r| r.item.map(|P2pItem::V30(mapping)| mapping))
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
            .find_map(|r| r.item.map(|PartyToKeyItem::V30(mapping)| mapping))
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
    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?);
    for key in &signing_keys {
        let fingerprint = utils::compute_fingerprint(key);
        let resp = vault_client
            .list_my_keys(tonic::Request::new(ListMyKeysRequest {
                filters: Some(ListKeysFilters {
                    fingerprint: fingerprint.clone(),
                    name: String::new(),
                    purpose: vec![],
                    usage_v30: vec![],
                }),
                base_request: None,
            }))
            .await?
            .into_inner();
        if !resp.private_keys_metadata.is_empty() {
            tracing::info!(
                "Recovered Daml signing key {fingerprint} for {dec_party_id} from the on-chain \
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
