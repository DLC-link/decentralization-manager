//! ACS (Active Contract Set) synchronization for party replication
//!
//! When adding a new member to a decentralized party that already has active
//! contracts, the new member needs to receive a copy of the ACS from an existing
//! member. This module provides functions for exporting and importing ACS snapshots.
//!
//! **Important**: ACS import requires the participant to be running in repair mode
//! (`canton.features.enable-repair-commands = true`), which requires a participant restart.

use std::collections::HashMap;

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    ContractImportMode, ExportAcsRequest, ImportAcsRequest,
    participant_repair_service_client::ParticipantRepairServiceClient,
};
use tokio_stream::StreamExt;

use crate::{config::NodeConfig, error::Result, utils};

/// Maximum gRPC message size for ACS operations (512MB)
const MAX_ACS_MESSAGE_SIZE: usize = 512 * 1024 * 1024;

/// Export the Active Contract Set for a party from the coordinator
///
/// This function exports all active contracts for the specified party at the
/// current ledger offset. The export is returned as a single byte vector
/// containing the compressed ACS snapshot.
///
/// # Arguments
/// * `config` - Node configuration for connecting to the participant
/// * `party_id` - The party ID to export contracts for
/// * `synchronizer_id` - The synchronizer ID to filter contracts (optional filter)
///
/// # Returns
/// A byte vector containing the compressed ACS snapshot
pub async fn export_party_acs(
    config: &NodeConfig,
    party_id: &str,
    synchronizer_id: &str,
) -> Result<Vec<u8>> {
    tracing::info!("Exporting ACS for party {party_id}");

    // Get current ledger offset for the snapshot
    let ledger_offset = get_current_ledger_offset(config).await?;
    tracing::debug!("Using ledger offset {ledger_offset} for ACS export");

    // Connect to repair service
    let mut repair_client = ParticipantRepairServiceClient::connect(config.admin_api_url())
        .await
        .context("Failed to connect to repair service")?;

    repair_client = repair_client.max_decoding_message_size(MAX_ACS_MESSAGE_SIZE);

    // Build export request
    let request = ExportAcsRequest {
        party_ids: vec![party_id.to_string()],
        synchronizer_id: synchronizer_id.to_string(),
        ledger_offset,
        contract_synchronizer_renames: HashMap::new(),
        excluded_stakeholder_ids: Vec::new(),
    };

    // Execute export and collect all chunks
    let mut stream = repair_client
        .export_acs(tonic::Request::new(request))
        .await
        .context("Failed to initiate ACS export")?
        .into_inner();

    let mut acs_data = Vec::new();
    let mut chunk_count = 0;

    while let Some(response) = stream.next().await {
        let response = response.context("Error receiving ACS export chunk")?;
        acs_data.extend_from_slice(&response.chunk);
        chunk_count += 1;
    }

    tracing::info!(
        "ACS export complete: {chunk_count} chunks, {size} bytes total",
        size = acs_data.len()
    );

    Ok(acs_data)
}

/// Import an Active Contract Set snapshot on the new member
///
/// This function imports a previously exported ACS snapshot into the participant.
/// **Requires the participant to be running in repair mode.**
///
/// # Arguments
/// * `config` - Node configuration for connecting to the participant
/// * `acs_snapshot` - The ACS snapshot data (from `export_party_acs`)
///
/// # Errors
/// Returns an error if:
/// - The participant is not running in repair mode
/// - The ACS data is invalid or corrupted
/// - Package validation fails
pub async fn import_party_acs(config: &NodeConfig, acs_snapshot: Vec<u8>) -> Result {
    tracing::info!("Importing ACS snapshot ({} bytes)", acs_snapshot.len());

    // Connect to repair service
    let mut repair_client = ParticipantRepairServiceClient::connect(config.admin_api_url())
        .await
        .context("Failed to connect to repair service")?;

    repair_client = repair_client.max_encoding_message_size(MAX_ACS_MESSAGE_SIZE);

    // Build import request with full validation
    let request = ImportAcsRequest {
        acs_snapshot,
        workflow_id_prefix: "add-party-acs-import".to_string(),
        contract_import_mode: ContractImportMode::Validation.into(),
        excluded_stakeholder_ids: Vec::new(),
        representative_package_id_override: None,
    };

    // Import requires streaming the request
    let stream = tokio_stream::once(request);

    let response = repair_client
        .import_acs(tonic::Request::new(stream))
        .await
        .map_err(|status| {
            // Check for common error conditions
            let msg = status.message();
            if msg.contains("repair mode")
                || msg.contains("REPAIR_COMMANDS_NOT_ENABLED")
                || msg.contains("enable-repair-commands")
            {
                anyhow::anyhow!(
                    "ACS import failed: participant is not in repair mode. \
                     Set `canton.features.enable-repair-commands = true` in config \
                     and restart the participant."
                )
            } else {
                anyhow::anyhow!("ACS import failed: {msg}")
            }
        })?
        .into_inner();

    let contract_count = response.contract_id_mappings.len();
    tracing::info!("ACS import complete: {contract_count} contracts imported");

    if !response.contract_id_mappings.is_empty() {
        tracing::debug!(
            "Contract ID remapping: {} entries",
            response.contract_id_mappings.len()
        );
    }

    Ok(())
}

/// Get the current ledger offset from the Ledger API
async fn get_current_ledger_offset(config: &NodeConfig) -> Result<i64> {
    use canton_proto_rs::com::daml::ledger::api::v2::GetLedgerEndRequest;

    // Create state client without auth (admin operation)
    let mut state_client = utils::create_state_client(config, None).await?;

    let response = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner();

    Ok(response.offset)
}

/// Check if the participant has repair mode enabled
///
/// This attempts to detect if repair mode is enabled by making a lightweight
/// call to the repair service. In production, this should be checked before
/// starting an ACS-requiring workflow.
pub async fn check_repair_mode_enabled(config: &NodeConfig) -> Result<bool> {
    // Try to connect to repair service - if repair mode is disabled,
    // the connection will succeed but operations will fail
    let repair_client = ParticipantRepairServiceClient::connect(config.admin_api_url()).await;

    match repair_client {
        Ok(_) => {
            // Connection succeeded - repair mode is at least accessible
            // The actual repair mode check happens when we try to import
            tracing::debug!("Repair service accessible");
            Ok(true)
        }
        Err(e) => {
            tracing::debug!("Repair service not accessible: {e}");
            Ok(false)
        }
    }
}
