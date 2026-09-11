use canton_proto_rs::com::digitalasset::canton::crypto::{
    admin::v30::vault_service_client::VaultServiceClient, v30::SigningKeyUsage,
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils::{compute_fingerprint, get_participant_id},
    workflow::{
        add_party::AddPartyConfig,
        onboarding::steps::generate_keys::{
            encode_keys_payload, get_or_create_signing_key, propose_namespace_delegation,
        },
        party_replication::capture_offset_once,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// New-member-only step: generate the namespace + Daml signing keys, propose
/// the namespace delegation, and persist everything the rest of the workflow
/// needs locally.
///
/// Mirrors onboarding's `generate_keys` (same key names, same idempotent
/// get-or-create path, same delegation proposal), plus one add-party-specific
/// extra: the participant's current ledger offset is captured BEFORE the
/// party gets activated here, because `ClearPartyOnboardingFlag` later needs
/// a `begin_offset_exclusive` that precedes the activation.
pub async fn generate_keys(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
    ledger_token: Option<&str>,
) -> Result {
    tracing::info!("Generating cryptographic keys for add-party...");

    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?);

    let namespace_key_name = add_party_config.namespace_key_name();
    let daml_key_name = add_party_config.daml_key_name();

    let (namespace_key, namespace_was_existing) = get_or_create_signing_key(
        &mut vault_client,
        &namespace_key_name,
        SigningKeyUsage::Namespace,
    )
    .await?;

    let namespace_fingerprint = compute_fingerprint(&namespace_key);
    tracing::debug!("Namespace key fingerprint: {namespace_fingerprint}");

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
