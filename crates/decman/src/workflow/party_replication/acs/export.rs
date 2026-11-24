use std::time::Duration;

use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    ExportPartyAcsRequest, party_management_service_client::PartyManagementServiceClient,
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    utils,
    workflow::party_replication::{ReplicationTarget, offset, pipe::ExportSession},
};

/// How long Canton may wait for the party's activation topology transaction
/// when exporting the ACS. The export runs after `SubmitProposals` already
/// confirmed the P2P in head state, so the activation is normally found
/// immediately; the timeout only covers replay lag.
const EXPORT_ACTIVATION_TIMEOUT_SECS: i64 = 120;

/// Source side: open an `ExportPartyAcs` stream for the target and wrap it in
/// a [`ExportSession`] the transport can pull blocks from.
///
/// Nothing is read here beyond the handshake: the snapshot is never assembled,
/// so a party of any size costs the source one block of memory. Canton locates
/// the party's activation on the target after `begin_offset_exclusive` (the
/// offset captured BEFORE the topology was submitted) and produces a snapshot
/// consistent with that activation.
///
/// # Errors
/// Returns an error if the export offset was never captured, or if Canton will
/// not open the export.
pub async fn open_export_session(
    config: &NodeConfig,
    storage: &SqlitePool,
    target: &ReplicationTarget,
) -> Result<ExportSession> {
    // Logical synchronizer id — see `current_ledger_offset` for why the
    // physical id is rejected by PartyManagementService.
    let synchronizer_id =
        utils::extract_synchronizer_fingerprint(&utils::get_synchronizer_id(config).await?)?;

    let begin_offset_exclusive = offset::persisted_or_derived_offset(
        config,
        storage,
        target,
        target.artifacts.export_offset,
        None,
    )
    .await?;

    tracing::info!(
        "Opening ACS export of {party} for target {member} (begin offset {begin_offset_exclusive})",
        party = target.party_id,
        member = target.target_participant_id
    );

    let mut client = PartyManagementServiceClient::new(config.admin_channel().await?);

    // Bounded retry on INVALID_STATE: the export locates the party's MOST
    // RECENT activation on the target in this participant's published
    // ledger-API events. When the target hosted the party before (removed,
    // now re-added), an OLD flag-less activation is already published while
    // the re-add's event may still be awaiting publication — Canton then
    // aborts with "must be activated … with the onboarding flag set"
    // instead of waiting. Publication catches up within seconds; retry
    // until the flagged re-add activation becomes the most recent one.
    let max_attempts = topology_retry_max_attempts();
    let retry_delay = Duration::from_secs(topology_retry_delay_secs());
    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ExportPartyAcsRequest {
            party_id: target.party_id.to_string(),
            synchronizer_id: synchronizer_id.clone(),
            target_participant_uid: target.target_participant_id.to_string(),
            begin_offset_exclusive,
            wait_for_activation_timeout: Some(prost_types::Duration {
                seconds: EXPORT_ACTIVATION_TIMEOUT_SECS,
                nanos: 0,
            }),
        });

        match client.export_party_acs(request).await {
            Ok(response) => return Ok(ExportSession::new(response.into_inner())),
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
