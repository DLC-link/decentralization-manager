use std::collections::HashSet;

use common::types::PackageInfo;
use sqlx::SqlitePool;

use crate::{
    config::{NodeConfig, Peer},
    db::schema::SchemaRead,
    error::Result,
    noise::{
        Message, MessageType, NoiseKeypair, parse_public_key,
        send_noise_message_with_chunked_response,
    },
    utils,
    workflow::{
        add_party::AddPartyConfig,
        party_replication::{capture_offset_once, collect_party_package_ids},
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
/// - the new member holds every package the party's contracts need
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

    check_new_member_packages(config, storage, add_party_config, ledger_token).await?;

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

/// Refuse the add before any topology is written if the new member is missing a
/// package the party's contracts need.
///
/// `import_party_acs` checks this too, but only at `SyncAcs` — by then
/// `SubmitProposals` has hosted the party on the new participant with the
/// onboarding marker, so it is already receiving the party's ledger traffic
/// with no ACS behind it. Every archive of a contract created before that lands
/// in its ACS journal with no activation to derive a reassignment counter from,
/// and those orphans make the participant fail its next reconnect.
async fn check_new_member_packages(
    config: &NodeConfig,
    storage: &SqlitePool,
    add_party_config: &AddPartyConfig,
    ledger_token: Option<&str>,
) -> Result {
    let party_id = add_party_config.decentralized_party_id.to_string();
    let required = collect_party_package_ids(config, &party_id, ledger_token).await?;
    if required.is_empty() {
        return Ok(());
    }

    let new_participant = &add_party_config.new_participant_id;
    let peer = storage
        .get_peer(&new_participant.to_string())
        .await?
        .ok_or_else(|| anyhow::anyhow!("No peer record for the new member {new_participant}"))?;
    let held = fetch_peer_package_ids(config, &peer).await?;

    let missing: Vec<&str> = required
        .iter()
        .map(String::as_str)
        .filter(|id| !held.contains(*id))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "the new member {new_participant} is missing {n} package(s) required by the \
             party's contracts — upload and vet the corresponding DAR(s) on it before adding \
             it, or its ACS import cannot be validated. Missing package ids: {missing:?}",
            n = missing.len()
        );
    }

    tracing::info!(
        "New member holds all {n} package(s) the party's contracts reference",
        n = required.len()
    );
    Ok(())
}

/// The package ids a peer reports over `ListPackages`.
async fn fetch_peer_package_ids(config: &NodeConfig, peer: &Peer) -> Result<HashSet<String>> {
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let psk = keypair.derive_psk(&parse_public_key(&peer.public_key)?);
    let identity = config.participant_id().to_string();

    let response = send_noise_message_with_chunked_response(
        &peer.address,
        peer.port,
        &psk,
        identity.as_bytes(),
        &Message::new_empty(MessageType::ListPackages),
        &config.noise_retry,
    )
    .await?;

    let message = Message::from_bytes(&response)?;
    if message.msg_type != MessageType::Data {
        anyhow::bail!(
            "the new member answered ListPackages with {msg_type:?}",
            msg_type = message.msg_type
        );
    }

    let packages: Vec<PackageInfo> = serde_json::from_slice(&message.payload)?;
    Ok(packages.into_iter().map(|p| p.package_id).collect())
}
