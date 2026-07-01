use std::time::Duration;

use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    ContractImportMode, DisconnectAllSynchronizersRequest, ExportPartyAcsRequest,
    ImportPartyAcsRequest, ReconnectSynchronizersRequest,
    party_management_service_client::PartyManagementServiceClient,
    synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    noise::MAX_CHUNKED_TOTAL_SIZE,
    utils,
    workflow::{
        add_party::AddPartyConfig,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// How long Canton may wait for the party's activation topology transaction
/// when exporting the ACS. The export runs after `SubmitProposals` already
/// confirmed the P2P in head state, so the activation is normally found
/// immediately; the timeout only covers replay lag.
const EXPORT_ACTIVATION_TIMEOUT_SECS: i64 = 120;

/// Size of each `ImportPartyAcsRequest` chunk. Stays comfortably below
/// Canton's default 4 MiB gRPC message cap.
const IMPORT_CHUNK_SIZE: usize = 1024 * 1024;

/// Coordinator side: export the party's ACS for replication onto the new
/// member, via the Canton 3.4 `ExportPartyAcs` admin endpoint. Canton locates
/// the party's activation on the new member after `begin_offset_exclusive`
/// (the offset persisted by ExportState BEFORE the topology was submitted)
/// and produces a snapshot consistent with that activation — this is what
/// fixes the old implementation's export-at-current-ledger-end gap.
///
/// Returns the raw snapshot bytes; empty when the party has no active
/// contracts (the import side skips on empty).
pub async fn export_party_acs(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result<Vec<u8>> {
    // Logical synchronizer id — see `current_ledger_offset` for why the
    // physical id is rejected by PartyManagementService.
    let synchronizer_id =
        utils::extract_synchronizer_fingerprint(&utils::get_synchronizer_id(config).await?)?;

    let offset_bytes = storage
        .read_artifact(instance_name, artifact_kinds::ADD_PARTY_EXPORT_OFFSET, None)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("ADD_PARTY_EXPORT_OFFSET artifact missing — did ExportState run?")
        })?;
    let begin_offset_exclusive: i64 = String::from_utf8(offset_bytes)?
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse export offset: {e}"))?;

    tracing::info!(
        "Exporting ACS of {party} for new member {member} (begin offset {begin_offset_exclusive})",
        party = add_party_config.decentralized_party_id,
        member = add_party_config.new_participant_id
    );

    let mut client = PartyManagementServiceClient::connect(config.admin_api_url()).await?;

    // Bounded retry on INVALID_STATE: the export locates the party's MOST
    // RECENT activation on the target in this participant's published
    // ledger-API events. When the new member was a member before (kicked,
    // now re-added), an OLD flag-less activation is already published while
    // the re-add's event may still be awaiting publication — Canton then
    // aborts with "must be activated … with the onboarding flag set"
    // instead of waiting. Publication catches up within seconds; retry
    // until the flagged re-add activation becomes the most recent one.
    let max_attempts = topology_retry_max_attempts();
    let retry_delay = Duration::from_secs(topology_retry_delay_secs());
    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ExportPartyAcsRequest {
            party_id: add_party_config.decentralized_party_id.to_string(),
            synchronizer_id: synchronizer_id.clone(),
            target_participant_uid: add_party_config.new_participant_id.to_string(),
            begin_offset_exclusive,
            wait_for_activation_timeout: Some(prost_types::Duration {
                seconds: EXPORT_ACTIVATION_TIMEOUT_SECS,
                nanos: 0,
            }),
        });

        match collect_export_stream(&mut client, request).await {
            Ok(snapshot) => {
                // Size cap is enforced mid-stream in `collect_export_stream`, so a
                // returned snapshot is always within the chunked-transfer limit.
                tracing::info!("Exported ACS snapshot: {len} bytes", len = snapshot.len());
                return Ok(snapshot);
            }
            Err(status)
                if status
                    .message()
                    .contains("INVALID_STATE_PARTY_MANAGEMENT_ERROR")
                    && attempt < max_attempts =>
            {
                tracing::warn!(
                    "ExportPartyAcs not ready (attempt {attempt}/{max_attempts}), \
                     retrying in {retry_delay:?}: {status}"
                );
                tokio::time::sleep(retry_delay).await;
            }
            Err(status) => return Err(status.into()),
        }
    }

    anyhow::bail!("ExportPartyAcs still not ready after {max_attempts} attempts")
}

/// Run one `ExportPartyAcs` call and collect the streamed chunks.
async fn collect_export_stream(
    client: &mut PartyManagementServiceClient<tonic::transport::Channel>,
    request: tonic::Request<ExportPartyAcsRequest>,
) -> std::result::Result<Vec<u8>, tonic::Status> {
    let mut stream = client.export_party_acs(request).await?.into_inner();
    let mut snapshot = Vec::new();
    while let Some(response) = stream.message().await? {
        snapshot.extend_from_slice(&response.chunk);
        // Enforce the chunked-transfer cap while streaming so an oversized party
        // can't accumulate unbounded memory (and OOM) before the export finishes
        // — abort as soon as the running total crosses the cap.
        if snapshot.len() > MAX_CHUNKED_TOTAL_SIZE {
            return Err(tonic::Status::out_of_range(format!(
                "Exported ACS snapshot exceeds the {MAX_CHUNKED_TOTAL_SIZE}-byte \
                 chunked-transfer cap — the new member cannot receive it over Noise. \
                 Raise MAX_CHUNKED_TOTAL_SIZE (with a memory-bound review) to replicate \
                 a party this large."
            )));
        }
    }
    Ok(snapshot)
}

/// New-member side: import the ACS snapshot via the Canton 3.4
/// `ImportPartyAcs` admin endpoint. No repair mode or restart is needed,
/// but Canton DOES require the participant to be disconnected from all
/// synchronizers for the duration of the import (refused otherwise with
/// `IMPORT_ACS_ERROR: There are still synchronizers connected`) — the party
/// itself stays suspended here via the Onboarding marker until the
/// flag-clearing round, so the brief disconnect is the only downtime.
pub async fn import_party_acs(
    config: &NodeConfig,
    add_party_config: &AddPartyConfig,
    snapshot: Vec<u8>,
) -> Result {
    if snapshot.is_empty() {
        tracing::info!("ACS snapshot is empty — nothing to import");
        return Ok(());
    }

    // Canton 3.5's ImportPartyAcsRequest requires the synchronizer (logical
    // id — see `current_ledger_offset` for the physical-id pitfall) and the
    // party being imported.
    let synchronizer_id =
        utils::extract_synchronizer_fingerprint(&utils::get_synchronizer_id(config).await?)?;
    let party_id = add_party_config.decentralized_party_id.to_string();

    let mut connectivity =
        SynchronizerConnectivityServiceClient::connect(config.admin_api_url()).await?;
    tracing::info!("Disconnecting from all synchronizers for the ACS import...");
    connectivity
        .disconnect_all_synchronizers(tonic::Request::new(DisconnectAllSynchronizersRequest {}))
        .await?;

    let import_result = run_import(config, &synchronizer_id, &party_id, snapshot).await;

    // ALWAYS reconnect — a participant left disconnected is a worse failure
    // mode than a failed import (which the peer step retries end-to-end).
    tracing::info!("Reconnecting to synchronizers...");
    let reconnect_result = connectivity
        .reconnect_synchronizers(tonic::Request::new(ReconnectSynchronizersRequest {
            ignore_failures: false,
        }))
        .await;

    import_result?;
    reconnect_result.map_err(|status| {
        anyhow::anyhow!("ACS imported but synchronizer reconnect failed: {status}")
    })?;

    tracing::info!("ACS snapshot imported successfully");
    Ok(())
}

/// The streamed `ImportPartyAcs` call, isolated so the caller can pair it
/// with the disconnect/reconnect bracket.
async fn run_import(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &str,
    snapshot: Vec<u8>,
) -> Result {
    tracing::info!(
        "Importing ACS snapshot ({len} bytes)...",
        len = snapshot.len()
    );

    let mut client = PartyManagementServiceClient::connect(config.admin_api_url()).await?;

    let requests: Vec<ImportPartyAcsRequest> = snapshot
        .chunks(IMPORT_CHUNK_SIZE)
        .map(|chunk| ImportPartyAcsRequest {
            acs_snapshot: chunk.to_vec(),
            synchronizer_id: Some(synchronizer_id.to_string()),
            workflow_id_prefix: Some("add-party-acs-import".to_string()),
            contract_import_mode: Some(ContractImportMode::Validation as i32),
            representative_package_id_override: None,
            party_id: Some(party_id.to_string()),
        })
        .collect();

    client
        .import_party_acs(tonic::Request::new(futures::stream::iter(requests)))
        .await?;
    Ok(())
}
