use canton_proto_rs::com::digitalasset::canton::crypto::{
    admin::v30::vault_service_client::VaultServiceClient, v30::SigningKeyUsage,
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    signing::custody,
    utils::{compute_fingerprint, get_participant_id, get_synchronizer_id},
    workflow::{
        add_party::AddPartyConfig,
        onboarding::steps::generate_keys::{
            encode_keys_payload, get_or_create_signing_key, propose_namespace_delegation,
            run_daml_key_name,
        },
        party_replication::capture_offset_once,
        storage::{WorkflowStorage, artifact_kinds},
        topology,
    },
};

/// New-member-only step: generate the namespace + Daml signing keys, propose
/// the namespace delegation, and persist everything the rest of the workflow
/// needs locally.
///
/// Mirrors onboarding's `generate_keys` (same key names, same idempotent
/// get-or-create path, a new Daml key for each run, same KMS requirement, same delegation proposal), plus
/// one add-party-specific extra: the participant's current ledger offset is
/// captured BEFORE the party gets activated here, because
/// `ClearPartyOnboardingFlag` later needs a `begin_offset_exclusive` that
/// precedes the activation.
pub async fn generate_keys(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
    ledger_token: Option<&str>,
) -> Result {
    tracing::info!("Generating cryptographic keys for add-party...");

    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?);
    custody::require_kms_participant(config, storage, &mut vault_client).await?;

    let namespace_key_name = add_party_config.namespace_key_name();

    let (namespace_key, namespace_was_existing) = get_or_create_signing_key(
        &mut vault_client,
        &namespace_key_name,
        SigningKeyUsage::Namespace,
    )
    .await?;

    let namespace_fingerprint = compute_fingerprint(&namespace_key);
    tracing::debug!("Namespace key fingerprint: {namespace_fingerprint}");
    custody::require_kms_key(config, &mut vault_client, &namespace_fingerprint).await?;

    if !namespace_was_existing {
        propose_namespace_delegation(config, &namespace_key, &namespace_fingerprint).await?;
    } else {
        tracing::info!(
            "Reusing existing namespace key {namespace_fingerprint}; \
             skipping namespace delegation proposal (already authorized)"
        );
    }

    // A former host that still owns the party keeps its key in the party's
    // key set, so hosting it again reuses that key. Any other member gets a
    // new key for this run.
    let party = &add_party_config.decentralized_party_id;
    let synchronizer_id = get_synchronizer_id(config).await?;
    let (_, head) = topology::fetch_p2p_mapping_at_head(config, &synchronizer_id, party).await?;
    let in_force = head
        .party_signing_keys
        .map(|keys| keys.keys)
        .unwrap_or_default();
    let daml_key =
        match custody::party_key_in_force(config, storage, &mut vault_client, party, &in_force)
            .await?
        {
            Some(key) => {
                tracing::info!(
                    "Reusing Daml key {fingerprint}, which {party} still carries for this node",
                    fingerprint = compute_fingerprint(&key)
                );
                key
            }
            None => {
                let name = run_daml_key_name(config, storage, instance_name, &party.prefix).await?;
                custody::new_daml_key(config, storage, &mut vault_client, &name, Some(party))
                    .await?
                    .public_key
            }
        };

    let self_id = config.participant_id().to_string();

    let keys_payload = encode_keys_payload(&namespace_key, &daml_key);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&self_id),
            &keys_payload,
        )
        .await?;

    let participant_id = get_participant_id(config).await?;
    tracing::info!("Participant ID: {participant_id}");
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::PARTICIPANT_ID,
            Some(&self_id),
            participant_id.to_file_format().as_bytes(),
        )
        .await?;

    capture_offset_once(
        config,
        storage,
        &add_party_config.replication_target(instance_name),
        artifact_kinds::ADD_PARTY_PRE_ACTIVATION_OFFSET,
        Some(&self_id),
        ledger_token,
        "pre-activation",
    )
    .await?;

    tracing::info!("Add-party keys persisted to workflow_artifacts");
    Ok(())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::{SigningKeysWithThreshold, SigningPublicKey},
        protocol::v30::PartyToParticipant,
    };

    use super::*;
    use crate::{
        canton_id::CantonId,
        db::MIGRATOR,
        signing::{
            custody::SoftwareKeys,
            fake_admin_api::{FakeAdminApi, TestNode, VaultKey},
            key_store::KeyStore,
        },
    };

    /// A refused add leaves nothing in the vault to clean up.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn refuses_a_participant_without_kms_before_making_a_key(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::software("participant", 1)]).await?;
        let node = fake.node(SoftwareKeys::Refused, false).await?;
        let add = AddPartyConfig::new(
            CantonId::parse(
                "party::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )?,
            node.config.participant_id().clone(),
            2,
            None,
            2,
            "party-add-party-1".to_string(),
        );

        let error = match generate_keys(&node.config, &db, "party-add-party-1", &add, None).await {
            Ok(()) => anyhow::bail!("add-party created keys without KMS"),
            Err(e) => format!("{e:#}"),
        };

        assert!(error.contains("software vault, not in a KMS"), "{error}");
        assert!(fake.generated().is_empty());
        Ok(())
    }

    const RUN: &str = "party-add-party-2";

    fn party() -> Result<CantonId> {
        CantonId::parse(
            "party::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
    }

    /// A node without KMS under the opt-out, which DecMan gave a Daml key
    /// for the party in an earlier run, under the name runs used before
    /// each run named its own key. The vault holds the node's namespace key
    /// for the party, so the run proposes no delegation.
    async fn former_host(db: &SqlitePool) -> Result<(FakeAdminApi, TestNode, SigningPublicKey)> {
        let fake = FakeAdminApi::start(vec![VaultKey::software("party-namespace", 1)]).await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;
        let earlier = custody::new_daml_key(
            &node.config,
            db,
            &mut VaultServiceClient::new(node.config.admin_channel().await?),
            &crate::workflow::signing_keys::party_daml_key_name("party"),
            None,
        )
        .await?
        .public_key;
        sqlx::query(
            "INSERT INTO workflow_runs (
                 instance_name, kind, role, status, current_step, step_index, step_total,
                 config_json, expected_peers_json, completed_peers_json,
                 created_at, updated_at
             ) VALUES (?1, 'AddParty', 'Peer', 'inprogress', 'GenerateKeys',
                       0, 1, '{}', '[]', '[]', 0, 0)",
        )
        .bind(RUN)
        .execute(db)
        .await?;
        // The ledger offset is already captured, so the step needs no ledger.
        db.write_artifact(
            RUN,
            artifact_kinds::ADD_PARTY_PRE_ACTIVATION_OFFSET,
            Some(&node.config.participant_id().to_string()),
            b"42",
        )
        .await?;
        Ok((fake, node, earlier))
    }

    fn hosting(keys: Vec<SigningPublicKey>) -> Result<PartyToParticipant> {
        Ok(PartyToParticipant {
            party: party()?.to_string(),
            threshold: 1,
            participants: Vec::new(),
            party_signing_keys: Some(SigningKeysWithThreshold { keys, threshold: 1 }),
        })
    }

    async fn add_this_node(node: &TestNode, db: &SqlitePool) -> Result<SigningPublicKey> {
        let add = AddPartyConfig::new(
            party()?,
            node.config.participant_id().clone(),
            2,
            None,
            1,
            RUN.to_string(),
        );
        generate_keys(&node.config, db, RUN, &add, None).await?;
        crate::workflow::onboarding::steps::generate_keys::own_new_daml_key(&node.config, db, RUN)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the run recorded no Daml key"))
    }

    async fn filed_under_party(
        node: &TestNode,
        db: &SqlitePool,
        key: &SigningPublicKey,
    ) -> Result<bool> {
        let store = KeyStore::open(&node.config, db.clone())
            .await?
            .ok_or_else(|| anyhow::anyhow!("the key store is off"))?;
        Ok(store
            .get(&party()?, &compute_fingerprint(key))
            .await?
            .is_some())
    }

    /// A former host that still owns the party keeps its key in the
    /// party's key set. Hosting it again reuses that key, or the party
    /// would carry two keys for one member.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_former_host_the_party_still_carries_gets_its_key_back(db: SqlitePool) -> Result {
        let (fake, node, earlier) = former_host(&db).await?;
        let peer = VaultKey::software("peer", 7).public_key;
        fake.publish_p2p(1, hosting(vec![peer, earlier.clone()])?);

        let key = add_this_node(&node, &db).await?;

        assert_eq!(key, earlier);
        assert!(filed_under_party(&node, &db, &key).await?);
        assert!(fake.authorized().is_empty(), "no delegation was proposed");
        assert!(fake.generated().is_empty(), "the vault made no key");
        Ok(())
    }

    /// A member whose key a kick or a rotation removed gets a new key, never
    /// the removed one. Each run names its own key, so even the name the
    /// earlier key was made under does not bring it back.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_member_added_again_gets_a_new_key(db: SqlitePool) -> Result {
        let (fake, node, earlier) = former_host(&db).await?;
        fake.publish_p2p(1, hosting(vec![VaultKey::software("peer", 7).public_key])?);

        let key = add_this_node(&node, &db).await?;

        assert_ne!(key, earlier);
        let name = String::from_utf8(
            db.read_artifact(
                RUN,
                artifact_kinds::DAML_KEY_NAME,
                Some(&node.config.participant_id().to_string()),
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("the run recorded no key name"))?,
        )?;
        assert!(name.starts_with("party-daml-transactions-run-"), "{name}");
        assert!(
            filed_under_party(&node, &db, &key).await?,
            "add-party knows the party, so it files the key at once"
        );
        assert!(fake.generated().is_empty(), "the vault made no key");
        Ok(())
    }
}
