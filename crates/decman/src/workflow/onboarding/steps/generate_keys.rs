use bytes::{BufMut, BytesMut};
use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{
            GenerateSigningKeyRequest, ListKeysFilters, ListMyKeysRequest,
            vault_service_client::VaultServiceClient,
        },
        v30::{SigningKeySpec, SigningKeyUsage, SigningPublicKey, public_key},
    },
    protocol::v30::{
        NamespaceDelegation, TopologyMapping, enums::TopologyChangeOp, namespace_delegation,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, StoreId, authorize_request, store_id,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils::{compute_fingerprint, get_participant_id},
    workflow::{
        onboarding::OnboardingConfig,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Generate cryptographic keys and export them
///
/// Generates:
/// 1. Namespace signing key (for namespace delegation)
/// 2. DAML transaction key (for signing transactions)
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

    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;

    // Derive key names from party_id_prefix
    let namespace_key_name = onboarding_config.namespace_key_name();
    let daml_key_name = onboarding_config.daml_key_name();

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

    // Only propose the namespace delegation when we just created the key — if
    // the key already existed, its delegation was authorized by the prior run
    // and re-proposing at the same serial would error.
    if !namespace_was_existing {
        propose_namespace_delegation(config, &namespace_key, &namespace_fingerprint).await?;
    } else {
        tracing::info!(
            "Reusing existing namespace key {namespace_fingerprint}; \
             skipping namespace delegation proposal (already authorized)"
        );
    }

    let (daml_key, _) =
        get_or_create_signing_key(&mut vault_client, &daml_key_name, SigningKeyUsage::Protocol)
            .await?;

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
async fn get_or_create_signing_key(
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
                usage: vec![usage as i32],
            }),
        }))
        .await?
        .into_inner();

    for meta in existing.private_keys_metadata {
        if let Some(pkn) = meta.public_key_with_name
            && let Some(pk) = pkn.public_key
            && let Some(public_key::Key::SigningPublicKey(spk)) = pk.key
        {
            tracing::info!("Reusing existing signing key with name '{name}'");
            return Ok((spk, true));
        }
    }

    tracing::debug!("Generating new signing key with name '{name}'");
    let response = vault_client
        .generate_signing_key(tonic::Request::new(GenerateSigningKeyRequest {
            key_spec: SigningKeySpec::EcCurve25519 as i32,
            name: name.to_string(),
            usage: vec![usage as i32],
        }))
        .await?
        .into_inner();
    let spk = response
        .public_key
        .ok_or_else(|| anyhow::anyhow!("No public key returned from VaultService"))?;
    Ok((spk, false))
}

/// Encode the namespace + DAML signing keys as two consecutive
/// `varint(len)||proto` messages — matches the on-disk format
/// `utils::write_messages_to_file(&[ns, daml], path)` produced, so the
/// coordinator's `read_all_messages_from_file` (now `read_all_messages` over
/// bytes) reads them back identically.
fn encode_keys_payload(namespace_key: &SigningPublicKey, daml_key: &SigningPublicKey) -> Vec<u8> {
    let mut buffer = BytesMut::new();
    for key in [namespace_key, daml_key] {
        let encoded = key.encode_to_vec();
        prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
        buffer.put_slice(&encoded);
    }
    buffer.to_vec()
}

/// Propose namespace delegation for the generated namespace key
async fn propose_namespace_delegation(
    config: &NodeConfig,
    namespace_key: &SigningPublicKey,
    namespace_fingerprint: &str,
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

    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::NamespaceDelegation(
                        namespace_delegation,
                    )),
                }),
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
