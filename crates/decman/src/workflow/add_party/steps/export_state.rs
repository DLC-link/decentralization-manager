use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        add_party::AddPartyConfig,
        party_replication::capture_offset_once,
        storage::{WorkflowStorage, artifact_kinds},
        topology,
    },
};

/// Coordinator step: export the party's current topology state and validate
/// the add against it.
///
/// Persists:
/// - `ADD_PARTY_NAMESPACE_DEF` — current `DecentralizedNamespaceDefinition`
/// - `ADD_PARTY_EXPORT_OFFSET` — this (source) participant's ledger offset,
///   captured BEFORE any topology change is submitted so `ExportPartyAcs`
///   can find the party's activation on the new member after it
///
/// Validates:
/// - the namespace and P2P mapping exist on the synchronizer
/// - the new participant is not already in the mapping
/// - `1 <= new_threshold <= current_owners + 1`
pub async fn export_state(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
    ledger_token: Option<&str>,
) -> Result {
    tracing::info!("Exporting current decentralized namespace state for add-party...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let namespace_hex = add_party_config.decentralized_party_id.namespace.to_hex();
    let namespace_def =
        topology::fetch_namespace_definition(config, &synchronizer_id, &namespace_hex).await?;

    tracing::info!(
        "Found namespace with {count} owners, threshold {threshold}",
        count = namespace_def.owners.len(),
        threshold = namespace_def.threshold
    );

    let party_id = &add_party_config.decentralized_party_id;
    let p2p_mapping = topology::fetch_p2p_mapping(config, &synchronizer_id, party_id).await?;

    let new_participant = &add_party_config.new_participant_id;
    if p2p_mapping
        .participants
        .iter()
        .any(|p| p.participant_uid == new_participant.to_string())
    {
        anyhow::bail!("Participant {new_participant} is already a member of {party_id}");
    }

    // Excludes the member being added: it carries the onboarding marker until
    // its ACS import completes and so cannot sign. A P2P add at threshold = the
    // post-add owner count is the known full-threshold bug, where the write
    // never becomes effective and the run stalls silently. Including the new
    // member is a separate change-threshold run once its marker clears.
    let signing_owner_count = namespace_def.owners.len() as i32;
    let new_threshold = add_party_config.new_threshold;
    if new_threshold < 1 || new_threshold > signing_owner_count {
        anyhow::bail!(
            "new_threshold must be between 1 and {signing_owner_count} (the current owners, \
             which can sign; the member being added cannot until its ACS import completes); got \
             {new_threshold}. To include it, run change-threshold once its onboarding marker \
             clears"
        );
    }

    let namespace_bytes = utils::encode_length_prefixed_message(&namespace_def);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_NAMESPACE_DEF,
            None,
            &namespace_bytes,
        )
        .await?;
    tracing::info!("Saved namespace definition to storage");

    capture_offset_once(
        config,
        storage,
        &add_party_config.replication_target(instance_name),
        artifact_kinds::ADD_PARTY_EXPORT_OFFSET,
        None,
        ledger_token,
        "pre-activation export",
    )
    .await?;

    tracing::info!("Add-party state exported successfully");
    Ok(())
}
