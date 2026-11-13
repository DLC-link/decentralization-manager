use bytes::{BufMut, BytesMut};
use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{
            GenerateSigningKeyRequest, ListKeysFilters, ListMyKeysRequest,
            generate_signing_key_response, private_key_metadata,
            vault_service_client::VaultServiceClient,
        },
        v30::{SigningKeySpec, SigningKeyUsage, SigningPublicKey, public_key},
    },
    protocol::v30::{
        NamespaceDelegation, TopologyMapping, enums::TopologyChangeOp, namespace_delegation,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, BaseQuery, ListNamespaceDelegationRequest, StoreId, authorize_request,
        base_query, store_id,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    canton_id::Fingerprint,
    config::NodeConfig,
    error::Result,
    signing::custody,
    utils::{compute_fingerprint, get_participant_id},
    workflow::{
        onboarding::OnboardingConfig,
        signing_keys,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Generate cryptographic keys and export them
///
/// Refuses a participant that does not keep its keys in KMS, unless this node
/// allows software keys (see [`custody`]). Under the opt-out, DecMan
/// generates the Daml key itself.
///
/// Generates:
/// 1. Namespace signing key (for namespace delegation)
/// 2. Daml transaction key (for signing transactions)
/// 3. Persists both keys + the participant id into `workflow_artifacts`
///    keyed by this node's own canton id, so the coordinator can later
///    aggregate all peers' keys via `list_artifacts`.
///
/// This function generates signing keys and exports them,
/// and proposes a namespace delegation for the generated namespace key.
pub async fn generate_keys(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    onboarding_config: &OnboardingConfig,
) -> Result {
    tracing::info!("Generating cryptographic keys...");

    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?);
    custody::require_kms_participant(config, storage, &mut vault_client).await?;

    // Derive key names from party_id_prefix
    let namespace_key_name = onboarding_config.namespace_key_name();
    let daml_key_name = run_daml_key_name(
        config,
        storage,
        instance_name,
        &onboarding_config.party_id_prefix,
    )
    .await?;

    // Idempotency: re-use existing keys (matched by name) if a previous run
    // already created them. Re-generating would mint new fingerprints, leaving
    // the previously-proposed namespace delegation pointing at a stale key
    // and breaking topology signing on retry.
    let (namespace_key, namespace_was_existing) = get_or_create_signing_key(
        &mut vault_client,
        &namespace_key_name,
        SigningKeyUsage::Namespace,
    )
    .await?;

    let namespace_fingerprint = compute_fingerprint(&namespace_key);
    tracing::debug!("Namespace key fingerprint: {namespace_fingerprint}");
    custody::require_kms_key(config, &mut vault_client, &namespace_fingerprint).await?;

    // Propose the namespace delegation unless the topology already carries it.
    // Re-proposing an authorized delegation errors, but keying the skip on
    // "the vault has the key" is not the same question: a run interrupted
    // between GenerateSigningKey and the authorize below leaves the key
    // present and the delegation absent, and every retry would then skip it —
    // silently dropping this node's namespace key out of Canton's signing-key
    // auto-selection for the rest of the party's life (#261). Ask the topology
    // instead. The check is skipped for a freshly-minted key, which cannot
    // have a delegation yet.
    if !namespace_was_existing
        || !namespace_delegation_exists(config, &namespace_fingerprint).await?
    {
        propose_namespace_delegation(config, &namespace_key, &namespace_fingerprint).await?;
    } else {
        tracing::info!(
            "Reusing existing namespace key {namespace_fingerprint}; \
             its namespace delegation is already authorized"
        );
    }

    // The party does not exist yet. A member whose key DecMan generated files
    // it under the party once it learns the party id.
    let daml_key = custody::new_daml_key(config, storage, &mut vault_client, &daml_key_name, None)
        .await?
        .public_key;

    // Persist keys + participant id under this node's own canton id so the
    // coordinator can later list every peer's keys/ids.
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
    tracing::info!("Keys persisted to workflow_artifacts");

    // Get and export participant ID from Canton
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
    tracing::info!("Participant ID persisted to workflow_artifacts");

    Ok(())
}

/// Look up an existing signing key by name, or create a new one if none
/// exists. Returns the public key and a flag indicating whether the key was
/// pre-existing (`true`) or freshly created (`false`).
///
/// This is what makes onboarding's GenerateKeys step idempotent across
/// retries — without it, every retry mints a new fingerprint and breaks the
/// topology delegation chain.
///
/// `pub(crate)` so the add-party workflow's new-member key generation reuses
/// the exact same idempotent path instead of duplicating it.
pub(crate) async fn get_or_create_signing_key(
    vault_client: &mut VaultServiceClient<tonic::transport::Channel>,
    name: &str,
    usage: SigningKeyUsage,
) -> Result<(SigningPublicKey, bool)> {
    let existing = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: String::new(),
                name: name.to_string(),
                purpose: vec![],
                usage_v30: vec![usage as i32],
            }),
            base_request: None,
        }))
        .await?
        .into_inner();

    for meta in existing.private_keys_metadata {
        if let Some(private_key_metadata::PublicKeyWithName::V30(pkn)) = meta.public_key_with_name
            && let Some(pk) = pkn.public_key
            && let Some(public_key::Key::SigningPublicKey(spk)) = pk.key
        {
            tracing::info!("Reusing existing signing key with name '{name}'");
            return Ok((spk, true));
        }
    }

    tracing::debug!("Generating new signing key with name '{name}'");
    // Leave the key spec UNSPECIFIED so the node picks its own configured
    // default: EC-Curve25519 (Ed25519) on the JCE provider, EC-P256 on a
    // KMS-backed node. Hardcoding EcCurve25519 broke party creation on
    // KMS nodes (AWS, MPCH), which treat Curve25519 as verification-only and
    // reject generation with SIGNING_KEY_GENERATION_ERROR / UnsupportedKeySpec.
    // Deferring to the node default also avoids decman requesting a spec that
    // the node's `crypto.signing.keys.allowed` set forbids. See #264.
    let response = vault_client
        .generate_signing_key(tonic::Request::new(GenerateSigningKeyRequest {
            key_spec: SigningKeySpec::Unspecified as i32,
            name: name.to_string(),
            usage_v30: vec![usage as i32],
            base_request: None,
        }))
        .await?
        .into_inner();
    let Some(generate_signing_key_response::PublicKey::V30(spk)) = response.public_key else {
        anyhow::bail!("No public key returned from VaultService");
    };
    Ok((spk, false))
}

/// The name of the Daml key this node creates in the run `instance_name`.
///
/// The first call picks a random name and records it with the run, so a
/// retry or a restart finds the same key. A new run gets a new key, even
/// when it reuses the instance name, because a finished run's records are
/// deleted. A run that recorded its keys before runs had their own names
/// keeps the earlier shared name, so it finds the key it recorded.
///
/// # Errors
///
/// Errors when the record cannot be read or written, or is not UTF-8.
pub(crate) async fn run_daml_key_name(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    party_id_prefix: &str,
) -> Result<String> {
    let self_id = config.participant_id().to_string();
    if let Some(name) = storage
        .read_artifact(instance_name, artifact_kinds::DAML_KEY_NAME, Some(&self_id))
        .await?
    {
        return Ok(String::from_utf8(name)?);
    }
    if storage
        .read_artifact(
            instance_name,
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&self_id),
        )
        .await?
        .is_some()
    {
        return Ok(signing_keys::party_daml_key_name(party_id_prefix));
    }
    let name = signing_keys::run_daml_key_name(
        party_id_prefix,
        &uuid::Uuid::new_v4().simple().to_string(),
    );
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::DAML_KEY_NAME,
            Some(&self_id),
            name.as_bytes(),
        )
        .await?;
    Ok(name)
}

/// The Daml key this node created for the run `instance_name`, from the key
/// bundle GenerateKeys recorded. `None` when this node created no keys in the
/// run, as for an existing member in add-party.
///
/// # Errors
///
/// Errors when the record cannot be read or does not hold two keys.
pub(crate) async fn own_new_daml_key(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
) -> Result<Option<SigningPublicKey>> {
    let self_id = config.participant_id().to_string();
    let Some(payload) = storage
        .read_artifact(
            instance_name,
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&self_id),
        )
        .await?
    else {
        return Ok(None);
    };
    let keys =
        crate::workflow::onboarding::steps::proposals::create::decode_keys_payload(&payload)?;
    match <[SigningPublicKey; 2]>::try_from(keys) {
        Ok([_, daml_key]) => Ok(Some(daml_key)),
        Err(keys) => anyhow::bail!(
            "This node's key record for {instance_name} holds {count} keys, not 2",
            count = keys.len()
        ),
    }
}

/// Encode the namespace + Daml signing keys as two consecutive
/// `varint(len)||proto` messages — matches the on-disk format
/// `utils::write_messages_to_file(&[ns, daml], path)` produced, so the
/// coordinator's `read_all_messages_from_file` (now `read_all_messages` over
/// bytes) reads them back identically.
pub(crate) fn encode_keys_payload(
    namespace_key: &SigningPublicKey,
    daml_key: &SigningPublicKey,
) -> Vec<u8> {
    let mut buffer = BytesMut::new();
    for key in [namespace_key, daml_key] {
        let encoded = key.encode_to_vec();
        prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
        buffer.put_slice(&encoded);
    }
    buffer.to_vec()
}

/// Is a namespace delegation for `namespace_fingerprint` — targeting that same
/// key, i.e. the root delegation [`propose_namespace_delegation`] issues —
/// already authorized in this participant's Authorized store?
///
/// The Authorized store is queried rather than the synchronizer store because
/// that is where the delegation is written; it answers "did we authorize it"
/// without depending on synchronizer propagation having caught up.
async fn namespace_delegation_exists(
    config: &NodeConfig,
    namespace_fingerprint: &Fingerprint,
) -> Result<bool> {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let response = topology_read_client
        .list_namespace_delegation(tonic::Request::new(ListNamespaceDelegationRequest {
            base_query: Some(BaseQuery {
                store: Some(StoreId {
                    store: Some(store_id::Store::Authorized(store_id::Authorized {})),
                }),
                proposals: false,
                operation: 0,
                time_query: Some(base_query::TimeQuery::HeadState(())),
                filter_signed_key: String::new(),
                protocol_version: None,
                client_version: None,
            }),
            filter_namespace: namespace_fingerprint.to_string(),
            filter_target_key_fingerprint: namespace_fingerprint.to_string(),
        }))
        .await?
        .into_inner();

    Ok(!response.results.is_empty())
}

/// Propose namespace delegation for the generated namespace key
///
/// `pub(crate)` so the add-party workflow's new-member key generation reuses
/// the same delegation path (see `get_or_create_signing_key`).
pub(crate) async fn propose_namespace_delegation(
    config: &NodeConfig,
    namespace_key: &SigningPublicKey,
    namespace_fingerprint: &Fingerprint,
) -> Result {
    tracing::debug!("Proposing namespace delegation for {namespace_fingerprint}");

    let namespace_delegation = NamespaceDelegation {
        // fingerprint of the root key defining the namespace
        namespace: namespace_fingerprint.to_string(),

        // target key of getting full rights on the namespace (if target == namespace, it's a root CA)
        target_key: Some(namespace_key.clone()),

        #[allow(deprecated)]
        is_root_delegation: false,

        // restricts target_key to only sign transactions with the specified mapping types.
        // for backwards compatibility, only the following combinations are valid:
        //
        // * is_root_delegation = true,  restriction = empty: the key can sign all mappings
        // * is_root_delegation = false, restriction = empty: the key can sign all mappings but namespace delegations
        // * is_root_delegation = false, restriction = non-empty: the key can only sign the mappings according the restriction that is set
        restriction: Some(namespace_delegation::Restriction::CanSignAllMappings(
            namespace_delegation::CanSignAllMappings {},
        )),
    };

    let mut topology_client = TopologyManagerWriteServiceClient::new(config.admin_channel().await?);

    let request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                protocol_version: None,
                change: TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::NamespaceDelegation(
                        namespace_delegation,
                    )),
                })),
            },
        )),
        must_fully_authorize: true,
        force_changes: vec![],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Authorized(store_id::Authorized {})),
        }),
        wait_to_become_effective: None,
    });

    topology_client.authorize(request).await?;
    tracing::debug!("Namespace delegation proposed successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::MIGRATOR,
        signing::{
            custody::SoftwareKeys,
            fake_admin_api::{FakeAdminApi, VaultKey},
            key_store::STORE_OFF_HINT,
        },
    };

    fn onboarding() -> OnboardingConfig {
        OnboardingConfig::new(
            "party".to_string(),
            "party-creation".to_string(),
            None,
            None,
        )
    }

    async fn refusal(fake: &FakeAdminApi, rule: SoftwareKeys, db: &SqlitePool) -> Result<String> {
        let node = fake.node(rule, false).await?;
        match generate_keys(&node.config, db, "party-creation", &onboarding()).await {
            Ok(()) => anyhow::bail!("onboarding created keys under {rule:?}"),
            Err(e) => Ok(format!("{e:#}")),
        }
    }

    /// A refused onboarding leaves nothing in the vault to clean up.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn refuses_a_participant_without_kms_before_making_a_key(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::wrapped("participant", 1)]).await?;

        let error = refusal(&fake, SoftwareKeys::Refused, &db).await?;

        assert!(error.contains("software vault, not in a KMS"), "{error}");
        assert!(fake.generated().is_empty());
        Ok(())
    }

    /// A run's records refer to its `workflow_runs` row.
    async fn insert_run(db: &SqlitePool, instance_name: &str) -> Result {
        sqlx::query(
            "INSERT INTO workflow_runs (
                 instance_name, kind, role, status, current_step, step_index, step_total,
                 config_json, expected_peers_json, completed_peers_json,
                 created_at, updated_at
             ) VALUES (?1, 'Onboarding', 'Coordinator', 'inprogress', 'GenerateKeys',
                       0, 1, '{}', '[]', '[]', 0, 0)",
        )
        .bind(instance_name)
        .execute(db)
        .await?;
        Ok(())
    }

    /// A retry finds the run's Daml key again. A new run with the same
    /// instance name gets a new key, so a member that a party adds again
    /// never gets back a key that a rotation replaced.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn each_run_names_its_own_daml_key(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::kms("participant", 1)?]).await?;
        let node = fake.node(SoftwareKeys::Refused, false).await?;
        insert_run(&db, "party-creation").await?;

        let first = run_daml_key_name(&node.config, &db, "party-creation", "party").await?;
        assert!(first.starts_with("party-daml-transactions-run-"), "{first}");
        assert!(signing_keys::is_party_daml_key_name(&first, "party"));
        let retried = run_daml_key_name(&node.config, &db, "party-creation", "party").await?;
        assert_eq!(retried, first);

        // A finished run's records are deleted.
        sqlx::query("DELETE FROM workflow_artifacts WHERE instance_name = ?")
            .bind("party-creation")
            .execute(&db)
            .await?;
        let next = run_daml_key_name(&node.config, &db, "party-creation", "party").await?;
        assert_ne!(next, first);
        assert!(signing_keys::is_party_daml_key_name(&next, "party"));
        Ok(())
    }

    /// A run that recorded its keys before runs had their own names keeps
    /// the earlier shared name. A new name would make it create a second
    /// key in the middle of the run.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_run_from_before_run_names_keeps_the_shared_name(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::kms("participant", 1)?]).await?;
        let node = fake.node(SoftwareKeys::Refused, false).await?;
        let self_id = node.config.participant_id().to_string();
        insert_run(&db, "party-creation").await?;
        db.write_artifact(
            "party-creation",
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&self_id),
            b"keys",
        )
        .await?;

        let name = run_daml_key_name(&node.config, &db, "party-creation", "party").await?;

        assert_eq!(name, "party-daml-transactions");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_opt_out_without_a_key_store_refuses_before_making_a_key(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::software("participant", 1)]).await?;

        let error = refusal(&fake, SoftwareKeys::Stored, &db).await?;

        assert!(error.contains(STORE_OFF_HINT), "{error}");
        assert!(fake.generated().is_empty());
        Ok(())
    }

    /// Under the opt-out, each onboarding run gets a Daml key that only
    /// DecMan holds. A retry of the run finds the same key and proposes no
    /// second delegation. A new run gets a new key. The vault makes only the
    /// namespace key, because the participant signs topology with it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn each_onboarding_run_gets_its_own_generated_daml_key(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![]).await?;
        let node = fake.node(SoftwareKeys::Stored, true).await?;
        insert_run(&db, "party-creation").await?;
        insert_run(&db, "party-creation-2").await?;
        let daml_key = |instance: &'static str| {
            let config = node.config.clone();
            let db = db.clone();
            async move {
                own_new_daml_key(&config, &db, instance)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("{instance} recorded no Daml key"))
            }
        };

        generate_keys(&node.config, &db, "party-creation", &onboarding()).await?;
        let first = daml_key("party-creation").await?;
        generate_keys(&node.config, &db, "party-creation", &onboarding()).await?;
        let retried = daml_key("party-creation").await?;
        let second_run = OnboardingConfig::new(
            "party".to_string(),
            "party-creation-2".to_string(),
            None,
            None,
        );
        generate_keys(&node.config, &db, "party-creation-2", &second_run).await?;
        let second = daml_key("party-creation-2").await?;

        assert_eq!(retried, first, "a retry finds the run's key");
        assert_ne!(second, first, "a new run gets a new key");
        assert_eq!(fake.generated(), vec!["party-namespace".to_string()]);
        assert_eq!(fake.authorized().len(), 1, "one namespace delegation");
        let generated: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM generated_party_keys")
            .fetch_one(&db)
            .await?;
        assert_eq!(generated, 2);
        Ok(())
    }
}
