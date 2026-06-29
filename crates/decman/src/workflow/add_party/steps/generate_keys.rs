use canton_proto_rs::com::{
    daml::ledger::api::v2::GetLedgerEndRequest,
    digitalasset::canton::{
        admin::participant::v30::{
            GetHighestOffsetByTimestampRequest,
            party_management_service_client::PartyManagementServiceClient,
        },
        crypto::{admin::v30::vault_service_client::VaultServiceClient, v30::SigningKeyUsage},
    },
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils::{self, compute_fingerprint, get_participant_id, get_synchronizer_id},
    workflow::{
        add_party::AddPartyConfig,
        onboarding::steps::generate_keys::{
            encode_keys_payload, get_or_create_signing_key, propose_namespace_delegation,
        },
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// New-member-only step: generate the namespace + DAML signing keys, propose
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

    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;

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

    // Capture this participant's pre-activation ledger offset. Idempotent on
    // retry by design: only the FIRST capture is kept, so a retry after the
    // topology already activated can't move the offset past the activation.
    let existing_offset = storage
        .read_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_PRE_ACTIVATION_OFFSET,
            Some(&self_id),
        )
        .await?;
    if existing_offset.is_none() {
        let offset = current_ledger_offset(config, ledger_token).await?;
        storage
            .write_artifact(
                instance_name,
                artifact_kinds::ADD_PARTY_PRE_ACTIVATION_OFFSET,
                Some(&self_id),
                offset.to_string().as_bytes(),
            )
            .await?;
        tracing::info!("Captured pre-activation ledger offset {offset}");
    }

    tracing::info!("Add-party keys persisted to workflow_artifacts");
    Ok(())
}

/// Current ledger offset on this participant — the `begin_offset_exclusive`
/// for the activation finders behind `ExportPartyAcs` and
/// `ClearPartyOnboardingFlag`.
///
/// The offset must postdate any EARLIER activation of the same (party,
/// participant) pair: the finders take the FIRST activation event published
/// after the offset, so on a kick-then-re-add path a too-early offset
/// surfaces the stale flag-less activation and the export aborts with
/// INVALID_STATE (observed in CI). Tiers:
///
/// 1. Ledger API `GetLedgerEnd`, with the party's ledger token when the
///    auth registry has one: exact and always current.
/// 2. Admin API `GetHighestOffsetByTimestamp` — strict, then `force: true`.
///    "Now" routinely trips the clean-watermark check, and even forced
///    lookups can fail when the latest events have no synchronizer mapping
///    (both observed in CI).
/// 3. Offset 1 — the smallest POSITIVE value the consumers accept. Loudly
///    warned: correct only when the participant was never hosted on the
///    party before (no stale activation to trip over).
pub(crate) async fn current_ledger_offset(
    config: &NodeConfig,
    ledger_token: Option<&str>,
) -> Result<i64> {
    match ledger_end_offset(config, ledger_token).await {
        Ok(offset) if offset > 0 => return Ok(offset),
        Ok(offset) => {
            tracing::warn!("Ledger end reported non-positive offset {offset}; trying admin API");
        }
        Err(e) => {
            tracing::warn!("GetLedgerEnd unavailable ({e}); trying admin API");
        }
    }

    // PartyManagementService wants the LOGICAL synchronizer id
    // (`alias::fingerprint`) — the physical id's trailing `::<version>`
    // fails Canton's fingerprint decoding with a reserved-delimiter error.
    let synchronizer_id =
        utils::extract_synchronizer_fingerprint(&get_synchronizer_id(config).await?)?;
    let mut client = PartyManagementServiceClient::connect(config.admin_api_url()).await?;

    for force in [false, true] {
        let now = std::time::SystemTime::now();
        let request = tonic::Request::new(GetHighestOffsetByTimestampRequest {
            synchronizer_id: synchronizer_id.clone(),
            timestamp: Some(prost_types::Timestamp::from(now)),
            force,
        });

        match client.get_highest_offset_by_timestamp(request).await {
            Ok(response) => {
                let offset = response.into_inner().ledger_offset;
                if offset > 0 {
                    return Ok(offset);
                }
                tracing::warn!(
                    "GetHighestOffsetByTimestamp (force: {force}) returned non-positive \
                     offset {offset}; retrying"
                );
            }
            Err(status) => {
                tracing::warn!("GetHighestOffsetByTimestamp (force: {force}) failed: {status}");
            }
        }
    }

    tracing::warn!(
        "No offset API usable on this participant; using offset 1 as \
         begin_offset_exclusive — UNSAFE if this participant hosted the party \
         before (a stale activation would be found first)"
    );
    Ok(1)
}

/// Ledger API ledger end. Authenticated when a token is supplied; the
/// tokenless form still works on deployments without ledger-API auth.
async fn ledger_end_offset(config: &NodeConfig, token: Option<&str>) -> Result<i64> {
    let mut client = utils::create_state_client(config, token.map(str::to_owned)).await?;
    let response = client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner();
    Ok(response.offset)
}
