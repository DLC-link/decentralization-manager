use canton_proto_rs::com::digitalasset::canton::{
    crypto::{admin::v30::vault_service_client::VaultServiceClient, v30::SigningPublicKey},
    protocol::v30::SignedTopologyTransaction,
    topology::admin::v30::SignTransactionsRequest,
};
use sqlx::SqlitePool;
use tokio::time;

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    db::schema::{Commitable, SchemaWrite},
    error::Result,
    signing::custody,
    utils::{self, compute_fingerprint},
    workflow::{
        onboarding::steps::{
            generate_keys::encode_keys_payload, proposals::create::decode_keys_payload,
        },
        proposal_store,
        rotate_key::{
            RotateKeyConfig,
            config::{require_complete_inline_keys, signing_threshold_in_force},
        },
        signing_keys::{adopt_legacy_signing_keys, own_namespace_key},
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
        topology,
    },
};

const LABEL: &str = "rotate-key";

/// Create the coordinator's new Daml key.
///
/// The key lives in KMS, unless this node allows participants without KMS.
/// DecMan then generates the key itself (see [`custody`]). A retry, or a new
/// run that replaces the same key, finds the key by its name, which depends
/// only on the replaced key.
///
/// # Errors
///
/// Errors when the vault or the key store refuses, when the key is not in
/// KMS and this node's rule refuses software keys, when DecMan cannot sign
/// with a new KMS key, or when the key is the one being replaced.
pub async fn generate_key(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    rotate: &RotateKeyConfig,
) -> Result {
    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?);
    custody::require_kms_participant(config, storage, &mut vault_client).await?;

    let name = rotate.new_key_name();
    let new = custody::new_daml_key(
        config,
        storage,
        &mut vault_client,
        &name,
        Some(&rotate.decentralized_party_id),
    )
    .await?;
    let key = new.public_key;
    let fingerprint = compute_fingerprint(&key);
    anyhow::ensure!(
        fingerprint != rotate.rotated_key,
        "The key under the name {name} is the key being replaced, {fingerprint}"
    );
    if let Some(kms_key_id) = new.kms_key_id.as_deref() {
        custody::require_kms_sign_access(&fingerprint, &key, kms_key_id).await?;
    }
    tracing::info!(
        "{verb} Daml key {fingerprint} ({name}) to replace {old} for {party}",
        verb = if new.existed { "Reusing" } else { "Created" },
        old = rotate.rotated_key,
        party = rotate.decentralized_party_id,
    );

    storage
        .write_artifact(
            instance_name,
            artifact_kinds::ROTATE_KEY_NEW_KEY,
            None,
            &utils::encode_length_prefixed_message(&key),
        )
        .await
}

/// Build the P2P proposal that swaps the coordinator's Daml key, and sign it.
///
/// The proposal is the current mapping with one key replaced. A party whose
/// keys still sit in a legacy `PartyToKeyMapping` has its members' keys moved
/// inline in the same proposal, as change-threshold does.
///
/// The coordinator signs with its namespace key and with the new key. Canton
/// requires the second signature as proof that the new key's holder agrees to
/// the key's addition. The vault adds it for a key it holds, and DecMan adds
/// it for a key it generated.
///
/// # Errors
///
/// Errors when the party carries fewer inline keys than hosts, when the
/// thresholds changed since the run started, when the party no longer
/// carries the key being replaced, or when Canton refuses to sign.
pub async fn create_proposal(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    rotate: &RotateKeyConfig,
) -> Result {
    let party = &rotate.decentralized_party_id;
    let new_key = read_new_key(storage, instance_name).await?;
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let head = proposal_store::fetch_party_head(config, &synchronizer_id, party).await?;
    require_complete_inline_keys(&head.p2p, party)?;
    let legacy = if head.p2p.party_signing_keys.is_none() {
        topology::fetch_party_to_key_mapping(config, &synchronizer_id, party).await?
    } else {
        None
    };
    let signing = signing_threshold_in_force(&head.p2p, legacy.as_ref())
        .and_then(|threshold| i32::try_from(threshold).ok());
    let confirmation = i32::try_from(head.p2p.threshold)?;
    if signing != Some(rotate.threshold) || confirmation != rotate.confirmation_threshold {
        anyhow::bail!(
            "{party}'s thresholds changed after this run started: signing {now} and \
             confirmation {confirmation}, against {then} and {then_conf}. Start the rotation \
             again so the invitation names the current ones",
            now = signing.map_or("none".to_string(), |t| t.to_string()),
            then = rotate.threshold,
            then_conf = rotate.confirmation_threshold,
        );
    }

    let members: Vec<String> = head
        .p2p
        .participants
        .iter()
        .map(|p| p.participant_uid.clone())
        .collect();
    let inline = head
        .p2p
        .party_signing_keys
        .clone()
        .map(|keys| keys.keys)
        .unwrap_or_default();
    let current = if inline.len() < members.len() {
        adopt_legacy_signing_keys(config, storage, &synchronizer_id, party, &members, &inline)
            .await?
    } else {
        inline
    };
    let keys = swap_key(&current, &rotate.rotated_key, new_key.clone())?;

    let p2p = rotate.thresholds().party_to_participant(
        party.to_string(),
        head.p2p.participants.clone(),
        keys,
    )?;
    let signed = proposal_store::sign_party_proposals(
        config,
        &synchronizer_id,
        instance_name,
        party,
        &head,
        None,
        Some(p2p),
        topology::party_proposal_force_flags(),
    )
    .await?;
    let mut proposal = signed
        .p2p
        .ok_or_else(|| anyhow::anyhow!("No rotate-key P2P proposal was signed"))?;
    custody::sign_with_generated_key(config, storage, &mut proposal, &new_key).await?;

    storage
        .write_artifact(
            instance_name,
            artifact_kinds::ROTATE_KEY_P2P_PROPOSAL,
            None,
            &utils::encode_length_prefixed_message(&proposal),
        )
        .await?;
    tracing::info!("Rotate-key proposal created and signed for {party}");
    Ok(())
}

/// `current` with `old` replaced by `new` in the same position.
///
/// # Errors
///
/// Errors when `current` does not carry `old`, or already carries `new`.
fn swap_key(
    current: &[SigningPublicKey],
    old: &Fingerprint,
    new: SigningPublicKey,
) -> Result<Vec<SigningPublicKey>> {
    let new_fingerprint = compute_fingerprint(&new);
    if current
        .iter()
        .any(|key| compute_fingerprint(key) == new_fingerprint)
    {
        anyhow::bail!("The party already carries the new key {new_fingerprint}");
    }
    let Some(position) = current
        .iter()
        .position(|key| compute_fingerprint(key) == *old)
    else {
        anyhow::bail!(
            "The party no longer carries the key {old} that this run replaces. Another change \
             removed it, so start the rotation again"
        );
    };
    let mut keys = current.to_vec();
    keys[position] = new;
    Ok(keys)
}

/// Sign the rotate-key proposal as a peer and keep the result for the
/// coordinator. The caller has already checked the proposal.
///
/// # Errors
///
/// Errors when the payload does not decode or Canton refuses to sign.
pub async fn sign_proposal(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal: &[u8],
) -> Result {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let transaction: SignedTopologyTransaction = utils::read_first_message_from_bytes(proposal)?;
    let request = SignTransactionsRequest {
        transactions: vec![transaction],
        signed_by: vec![],
        store: Some(topology::synchronizer_store_id(&synchronizer_id)),
        force_flags: vec![],
    };
    let response = topology::sign_transactions_with_topology_retry(config, request, LABEL).await?;
    let [signed] = response.transactions.as_slice() else {
        anyhow::bail!(
            "Expected 1 signed rotate-key transaction, got {count}",
            count = response.transactions.len()
        );
    };

    let node_id = config.participant_id().to_string();
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::SIGNED_ROTATE_KEY_P2P,
            Some(&node_id),
            &utils::encode_length_prefixed_message(signed),
        )
        .await?;
    tracing::info!("Rotate-key proposal signed");
    Ok(())
}

/// Add every peer's signature to the proposal, submit it, wait until the
/// party carries the new key, and record the new key as this node's own.
///
/// # Errors
///
/// Errors when an artefact is missing, when the new key's own signature is
/// missing, or when the topology does not change in time.
pub async fn submit_rotation(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    rotate: &RotateKeyConfig,
) -> Result {
    let party = &rotate.decentralized_party_id;
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let proposal = storage
        .read_artifact(instance_name, artifact_kinds::ROTATE_KEY_P2P_PROPOSAL, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("ROTATE_KEY_P2P_PROPOSAL artifact missing"))?;
    let mut transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&proposal)?;
    for (peer_id, signed) in storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_ROTATE_KEY_P2P)
        .await?
    {
        let signed: SignedTopologyTransaction = utils::read_first_message_from_bytes(&signed)?;
        tracing::info!("Adding the rotate-key signatures of {peer_id}");
        transaction.signatures.extend(signed.signatures);
    }
    topology::dedupe_signatures(&mut transaction);
    topology::check_added_signing_keys_signed(config, &synchronizer_id, party, &transaction)
        .await?;

    let new_key = read_new_key(storage, instance_name).await?;
    let new_fingerprint = compute_fingerprint(&new_key);
    topology::submit_p2p(
        config,
        &synchronizer_id,
        LABEL,
        transaction,
        topology::party_proposal_force_flags(),
        || {
            wait_for_rotation(
                config,
                &synchronizer_id,
                party,
                &rotate.rotated_key,
                &new_fingerprint,
            )
        },
    )
    .await?;

    record_own_key(config, storage, party, &synchronizer_id, new_key).await?;
    tracing::info!(
        "{party} now carries Daml key {new_fingerprint} in place of {old}",
        old = rotate.rotated_key
    );
    Ok(())
}

/// Wait until the party's head mapping carries `new` and no longer `old`.
async fn wait_for_rotation(
    config: &NodeConfig,
    synchronizer_id: &str,
    party: &CantonId,
    old: &Fingerprint,
    new: &Fingerprint,
) -> Result {
    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());
    for attempt in 1..=max_attempts {
        let (_, mapping) =
            topology::fetch_p2p_mapping_at_head(config, synchronizer_id, party).await?;
        if carries_rotation(
            &mapping
                .party_signing_keys
                .map(|k| k.keys)
                .unwrap_or_default(),
            old,
            new,
        ) {
            tracing::info!("Key rotation confirmed in topology after {attempt} attempt(s)");
            return Ok(());
        }
        if attempt < max_attempts {
            time::sleep(retry_delay).await;
        }
    }
    anyhow::bail!(
        "{party} did not carry the new key {new} in place of {old} after {max_attempts} attempts"
    )
}

fn carries_rotation(keys: &[SigningPublicKey], old: &Fingerprint, new: &Fingerprint) -> bool {
    let fingerprints: Vec<Fingerprint> = keys.iter().map(compute_fingerprint).collect();
    fingerprints.contains(new) && !fingerprints.contains(old)
}

/// Record `new_key` as this node's Daml key for `party`.
///
/// The deploy signer and the key-store copy task read this row first, so it
/// must name the new key before the next deploy. The namespace key is kept
/// from the old row, or else resolved from the vault.
async fn record_own_key(
    config: &NodeConfig,
    storage: &SqlitePool,
    party: &CantonId,
    synchronizer_id: &str,
    new_key: SigningPublicKey,
) -> Result {
    let self_id = config.participant_id().to_string();
    let recorded = match storage
        .read_identity(party, identity_kinds::PEER_PUBLIC_KEYS, &self_id)
        .await?
    {
        Some(payload) => decode_keys_payload(&payload)?,
        None => Vec::new(),
    };
    let namespace_key = match recorded.as_slice() {
        [namespace, _] => namespace.clone(),
        _ => own_namespace_key(config, party, synchronizer_id)
            .await?
            .unwrap_or_else(|| {
                tracing::warn!(
                    "No namespace key for {party} in this node's vault; recording the Daml key \
                     in its place, as a bundle recovered from the chain does"
                );
                new_key.clone()
            }),
    };
    storage
        .write_identity(
            party,
            identity_kinds::PEER_PUBLIC_KEYS,
            &self_id,
            &encode_keys_payload(&namespace_key, &new_key),
        )
        .await
}

/// The new key a `RecordRotatedKey` command carries.
///
/// # Errors
///
/// Errors when the payload does not decode.
pub fn decode_recorded_key(payload: &[u8]) -> Result<SigningPublicKey> {
    utils::read_first_message_from_bytes(payload)
}

/// Record the coordinator's new Daml key on a peer.
///
/// The key must be the one this peer checked in SignProposal (`accepted`),
/// so a coordinator cannot have it record another member's key. The peer
/// also checks that the party carries the key and no longer the one its
/// operator accepted to replace. The identity row is updated only when
/// this node holds one for the coordinator, which is the case on the node
/// that coordinated onboarding or added the coordinator.
///
/// # Errors
///
/// Errors when the topology does not show the rotation, or a write fails.
pub async fn record_peer_key(
    config: &NodeConfig,
    storage: &SqlitePool,
    party: &CantonId,
    coordinator: &CantonId,
    (rotated_key, accepted): (&Fingerprint, &Fingerprint),
    new_key: SigningPublicKey,
) -> Result {
    let new_fingerprint = compute_fingerprint(&new_key);
    if new_fingerprint != *accepted {
        anyhow::bail!(
            "The coordinator asks this node to record {new_fingerprint}, but this node checked \
             {accepted} as the rotation's new key"
        );
    }
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let (_, mapping) = topology::fetch_p2p_mapping_at_head(config, &synchronizer_id, party).await?;
    if !carries_rotation(
        &mapping
            .party_signing_keys
            .map(|k| k.keys)
            .unwrap_or_default(),
        rotated_key,
        &new_fingerprint,
    ) {
        anyhow::bail!(
            "{party} does not carry {new_fingerprint} in place of {rotated_key}, so this node \
             does not record it for {coordinator}"
        );
    }

    let coordinator_id = coordinator.to_string();
    if let Some(payload) = storage
        .read_identity(party, identity_kinds::PEER_PUBLIC_KEYS, &coordinator_id)
        .await?
    {
        match decode_keys_payload(&payload)?.as_slice() {
            [namespace, _] => {
                storage
                    .write_identity(
                        party,
                        identity_kinds::PEER_PUBLIC_KEYS,
                        &coordinator_id,
                        &encode_keys_payload(namespace, &new_key),
                    )
                    .await?;
            }
            keys => tracing::warn!(
                "PEER_PUBLIC_KEYS for {coordinator} on {party} holds {count} keys, not 2; \
                 leaving it as it is",
                count = keys.len()
            ),
        }
    }

    let mut tx = storage.begin_transaction().await?;
    tx.update_participant_signing_key(party, &coordinator_id, &new_fingerprint)
        .await?;
    Commitable::commit(tx).await?;
    tracing::info!("Recorded {coordinator}'s new Daml key {new_fingerprint} for {party}");
    Ok(())
}

async fn read_new_key(storage: &SqlitePool, instance_name: &str) -> Result<SigningPublicKey> {
    let bytes = storage
        .read_artifact(instance_name, artifact_kinds::ROTATE_KEY_NEW_KEY, None)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("ROTATE_KEY_NEW_KEY artifact missing — did GenerateKey run?")
        })?;
    utils::read_first_message_from_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::SigningKeyUsage;

    use super::*;

    fn key(seed: u8) -> SigningPublicKey {
        SigningPublicKey {
            public_key: vec![seed; 32],
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        }
    }

    fn fingerprints(keys: &[SigningPublicKey]) -> Vec<Fingerprint> {
        keys.iter().map(compute_fingerprint).collect()
    }

    #[test]
    fn swaps_the_old_key_in_place() -> Result {
        let current = vec![key(1), key(2), key(3)];
        let swapped = swap_key(&current, &compute_fingerprint(&key(2)), key(9))?;
        assert_eq!(
            fingerprints(&swapped),
            fingerprints(&[key(1), key(9), key(3)])
        );
        Ok(())
    }

    #[test]
    fn refuses_when_the_old_key_is_gone() {
        let current = vec![key(1), key(3)];
        let error = swap_key(&current, &compute_fingerprint(&key(2)), key(9))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("no longer carries"), "{error}");
    }

    /// Swapping in a key the party already carries would leave it one key
    /// short, with two members counted on one key.
    #[test]
    fn refuses_a_new_key_the_party_already_carries() {
        let current = vec![key(1), key(2)];
        let error = swap_key(&current, &compute_fingerprint(&key(2)), key(1))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("already carries"), "{error}");
    }

    /// GenerateKey against a fake participant.
    mod generate_key {
        use super::*;
        use crate::{
            db::MIGRATOR,
            server::{WorkflowKind, WorkflowRole},
            signing::{
                custody::SoftwareKeys,
                fake_admin_api::{FakeAdminApi, VaultKey},
                key_store::KeyStore,
            },
            workflow::signing_keys::party_daml_key_name,
        };

        const RUN: &str = "party-rotate-key-1";

        fn party() -> Result<CantonId> {
            CantonId::parse(
                "party::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
        }

        /// The coordinator's current Daml key, under its onboarding name.
        fn old_key() -> Result<VaultKey> {
            Ok(VaultKey::software(
                &party_daml_key_name(&party()?.prefix),
                1,
            ))
        }

        /// A rotation of `old` on a run row the artifacts can hang off.
        async fn rotation(db: &SqlitePool, old: &VaultKey) -> Result<RotateKeyConfig> {
            sqlx::query(
                "INSERT INTO workflow_runs
                 (instance_name, kind, role, status, current_step, step_index, step_total,
                  config_json, expected_peers_json, completed_peers_json, created_at, updated_at)
                 VALUES (?, ?, ?, 'inprogress', 'GenerateKey', 0, 7, '{}', '[]', '[]', 0, 0)",
            )
            .bind(RUN)
            .bind(WorkflowKind::RotateKey.as_str())
            .bind(WorkflowRole::Coordinator.as_str())
            .execute(db)
            .await?;
            Ok(RotateKeyConfig {
                decentralized_party_id: party()?,
                rotated_key: old.fingerprint(),
                threshold: 2,
                confirmation_threshold: 2,
                instance_name: RUN.to_string(),
            })
        }

        async fn new_key_artifact(db: &SqlitePool) -> Result<Option<SigningPublicKey>> {
            db.read_artifact(RUN, artifact_kinds::ROTATE_KEY_NEW_KEY, None)
                .await?
                .map(|bytes| utils::read_first_message_from_bytes(&bytes))
                .transpose()
        }

        fn refusal(result: Result) -> Result<String> {
            match result {
                Ok(()) => anyhow::bail!("expected GenerateKey to refuse"),
                Err(e) => Ok(format!("{e:#}")),
            }
        }

        async fn store(
            node: &crate::signing::fake_admin_api::TestNode,
            db: &SqlitePool,
        ) -> Result<KeyStore> {
            KeyStore::open(&node.config, db.clone())
                .await?
                .ok_or_else(|| anyhow::anyhow!("the key store is off"))
        }

        /// The new key's name comes from the key it replaces, so a second run
        /// finds the key the first one made.
        #[sqlx::test(migrator = "MIGRATOR")]
        async fn names_the_new_key_after_the_replaced_one_and_reuses_it(db: SqlitePool) -> Result {
            let old = old_key()?;
            let fake = FakeAdminApi::start(vec![old.clone()]).await?;
            let node = fake.node(SoftwareKeys::Stored, true).await?;
            let rotate = rotation(&db, &old).await?;
            let name = format!("party-daml-transactions-rotated-from-{}", old.fingerprint());

            generate_key(&node.config, &db, RUN, &rotate).await?;
            let first = new_key_artifact(&db).await?;
            generate_key(&node.config, &db, RUN, &rotate).await?;

            let generated = store(&node, &db).await?.generated_keys().await?;
            let [(generated_name, fingerprint)] = generated.as_slice() else {
                anyhow::bail!("expected one generated key, got {generated:?}");
            };
            assert_eq!(generated_name, &name);
            assert_ne!(fingerprint, &old.fingerprint());
            let new = new_key_artifact(&db).await?;
            assert_eq!(new, first);
            assert_eq!(
                new.as_ref().map(compute_fingerprint).as_ref(),
                Some(fingerprint)
            );
            Ok(())
        }

        /// Under the opt-out, DecMan generates the new key and files it for
        /// the party at once, so it can sign deploys. Canton never holds the
        /// key, so key download plays no part.
        #[sqlx::test(migrator = "MIGRATOR")]
        async fn generates_the_new_key_in_decman_with_key_download_off(db: SqlitePool) -> Result {
            let old = old_key()?;
            let fake = FakeAdminApi::start(vec![old.clone()]).await?;
            fake.turn_off_key_download();
            let node = fake.node(SoftwareKeys::Stored, true).await?;
            let rotate = rotation(&db, &old).await?;

            generate_key(&node.config, &db, RUN, &rotate).await?;

            assert!(fake.generated().is_empty());
            assert!(fake.exported().is_empty());
            let new = new_key_artifact(&db)
                .await?
                .ok_or_else(|| anyhow::anyhow!("no new key"))?;
            let store = store(&node, &db).await?;
            assert!(
                store
                    .get(&party()?, &compute_fingerprint(&new))
                    .await?
                    .is_some()
            );
            Ok(())
        }

        #[sqlx::test(migrator = "MIGRATOR")]
        async fn refuses_a_participant_without_kms_before_making_a_key(db: SqlitePool) -> Result {
            let old = old_key()?;
            let fake = FakeAdminApi::start(vec![old.clone()]).await?;
            let node = fake.node(SoftwareKeys::Refused, true).await?;
            let rotate = rotation(&db, &old).await?;

            let error = refusal(generate_key(&node.config, &db, RUN, &rotate).await)?;

            assert!(error.contains("software vault, not in a KMS"), "{error}");
            assert!(fake.generated().is_empty());
            assert_eq!(new_key_artifact(&db).await?, None);
            Ok(())
        }

        /// A vault that holds a KMS key may still hold a software key under
        /// the new key's name, made before the participant moved to KMS.
        #[sqlx::test(migrator = "MIGRATOR")]
        async fn refuses_a_new_key_outside_kms(db: SqlitePool) -> Result {
            let old = old_key()?;
            let probe = rotation(&db, &old).await?;
            let fake = FakeAdminApi::start(vec![
                old.clone(),
                VaultKey::kms("participant", 2)?,
                VaultKey::software(&probe.new_key_name(), 3),
            ])
            .await?;
            let node = fake.node(SoftwareKeys::Refused, true).await?;
            let rotate = probe;

            let error = refusal(generate_key(&node.config, &db, RUN, &rotate).await)?;

            assert!(error.contains("is in Canton's software vault"), "{error}");
            assert_eq!(new_key_artifact(&db).await?, None);
            Ok(())
        }

        /// Insecure mode lets the vault make a software key, and DecMan keeps
        /// no copy of it.
        #[sqlx::test(migrator = "MIGRATOR")]
        async fn insecure_mode_accepts_a_software_key_without_a_copy(db: SqlitePool) -> Result {
            let old = old_key()?;
            let fake = FakeAdminApi::start(vec![old.clone()]).await?;
            let node = fake.node(SoftwareKeys::Allowed, false).await?;
            let rotate = rotation(&db, &old).await?;

            generate_key(&node.config, &db, RUN, &rotate).await?;

            assert!(fake.exported().is_empty());
            assert!(new_key_artifact(&db).await?.is_some());
            Ok(())
        }
    }

    #[test]
    fn a_rotation_lands_only_when_the_new_key_replaces_the_old_one() {
        let old = compute_fingerprint(&key(2));
        let new = compute_fingerprint(&key(9));
        assert!(carries_rotation(&[key(1), key(9)], &old, &new));
        assert!(!carries_rotation(&[key(1), key(2)], &old, &new));
        assert!(!carries_rotation(&[key(1), key(2), key(9)], &old, &new));
    }
}
