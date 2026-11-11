use std::{future::Future, time::Duration};

use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    DisconnectSynchronizerRequest, ListConnectedSynchronizersRequest,
    list_connected_synchronizers_response,
    synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
};
use sqlx::SqlitePool;
use tonic::transport::Channel;

use super::{
    packages::local_package_ids,
    stream::{ImportError, ImportParams, give_up_transfer, pull_first_block, run_import},
    window::{close_import_window, synchronizer_healthy, verify_healthy},
};
use crate::{
    config::NodeConfig,
    consts::acs_import_attempts_in_window,
    db::schema::SchemaRead,
    error::Result,
    utils,
    workflow::party_replication::{ReplicationTarget, pipe::PipeBlock},
};

/// Pause before restarting a broken transfer, so a cancelled `ImportPartyAcs`
/// has unwound on the participant before the next one opens. The restart
/// budget itself is [`acs_import_attempts_in_window`]: the whole retry budget
/// of the step, because the participant is reconnected exactly once, after the
/// import succeeded or the last attempt failed, never between attempts. Every
/// reconnect replays the ACS journal, and a participant that has been hosting
/// the party without its ACS has rows in that journal that make the replay
/// fatal. Re-feeding the same snapshot over a partial import is safe because
/// Canton skips contracts that are already active.
const IMPORT_RETRY_DELAY: Duration = Duration::from_secs(10);

/// Payload of the in-flight marker when the window was opened before the party
/// was authorized onto this participant, which is the order Canton documents.
/// The single reconnect at the end of the import reads it to hand the
/// synchronizer back to automatic connection.
pub(super) const DISCONNECT_FIRST_MARKER: &[u8] = b"disconnect-first";

/// How long a re-entered import waits before trusting an empty connection list.
/// A participant that has just restarted reconnects on its own within seconds;
/// importing into it while that happens is refused by Canton.
const REENTRY_GRACE: Duration = Duration::from_secs(5);

/// Where the blocks of a transfer come from, which decides what a failed pull
/// may do about it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferSource {
    /// The coordinator's open export. A block can be asked for again, since the
    /// source replays the one it served last, and asking for block 1 again
    /// makes it re-open the export from the top.
    Coordinator,
    /// A wallet relaying blocks one way. Nothing can be asked for again, so the
    /// first broken pull ends the attempt and the wallet decides about a retry.
    Relay,
}

/// Target side: import the ACS snapshot via the Canton `ImportPartyAcs`
/// admin endpoint. Canton requires the participant to be disconnected from all
/// synchronizers for the duration of the import (refused otherwise with
/// `IMPORT_ACS_ERROR: There are still synchronizers connected`) — the party
/// itself stays suspended here via the Onboarding marker until the
/// flag-clearing round, so the disconnect is the only downtime.
///
/// The disconnect window is the fragile part: if the participant shuts down
/// uncleanly mid-import it can be left with orphan ACS rows that make it
/// FATAL-crash on every reconnect. DecMan can't make Canton's import atomic,
/// but it makes the window crash-safe on its own side:
///
/// - a durable marker is written before disconnecting, so a retry (after a
///   DecMan or participant crash) knows the window was opened by this run; a
///   participant found still disconnected is not reconnected, the import simply
///   continues inside the window it is already in;
/// - a transfer that breaks is restarted from block 1 inside the same
///   disconnect window, up to [`acs_import_attempts_in_window`] times; the
///   participant reconnects exactly once, after the import succeeded or the
///   last attempt failed, never between attempts;
/// - the participant is never left disconnected: that single reconnect runs
///   even when the import fails, and success is reported only once the
///   synchronizer is confirmed healthy again (not merely that
///   `ReconnectSynchronizers` was accepted);
/// - a participant that can't be brought back to a healthy connected state
///   yields an actionable error naming the likely orphan-row corruption instead
///   of a cryptic retry-abort.
pub async fn import_party_acs<F, Fut>(
    config: &NodeConfig,
    storage: &SqlitePool,
    target: &ReplicationTarget,
    required_package_ids: &[String],
    source: TransferSource,
    mut next_block: F,
) -> Result
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<PipeBlock>>,
{
    let window = read_window_marker(storage, target).await?;
    refuse_if_quarantined(storage, target).await?;

    // Pull the first block before touching the participant. It costs one
    // block of memory and answers the only question that decides whether a
    // disconnect is warranted at all: whether the party has any contracts.
    let first = next_block(1).await?;
    if let PipeBlock::End { trailer, .. } = &first
        && trailer.total_len == 0
    {
        tracing::info!("ACS snapshot is empty — nothing to import");
        // A window opened before the authorization is closed all the same: the
        // participant is off the synchronizer and on manual connection.
        if window.opened_before_authorization {
            close_import_window(config, true).await?;
        }
        return Ok(());
    }

    check_packages_present(config, required_package_ids).await?;
    let plan = preflight(config, target, window).await?;
    run_in_disconnect_window(
        config,
        storage,
        target,
        plan,
        source,
        first,
        &mut next_block,
    )
    .await
}

/// What the disconnect-window marker says on entry.
#[derive(Clone, Copy)]
struct WindowMarker {
    /// A prior attempt of this run entered the window.
    disconnect_window_opened: bool,
    /// The window was opened before the party was authorized onto this
    /// participant.
    opened_before_authorization: bool,
}

/// Read the disconnect-window marker of `target`.
async fn read_window_marker(
    storage: &SqlitePool,
    target: &ReplicationTarget,
) -> Result<WindowMarker> {
    // The marker is durable (never cleared), so its presence means the
    // disconnect window was entered on a prior attempt of this run — the
    // participant may have been left disconnected (DecMan died mid-window) or
    // crash-looping (unclean participant shutdown). Recover conservatively:
    // reconnect and verify health before doing anything else. This is a no-op
    // when the participant is already connected and healthy.
    // Through the target, not the pool: a tenant replication has no workflow
    // run, so its artefacts live in a table without the run foreign key.
    let window_marker = target
        .read_artifact(storage, target.artifacts.import_inflight, None)
        .await?;
    let disconnect_window_opened = window_marker.is_some();
    let opened_before_authorization = window_marker.as_deref() == Some(DISCONNECT_FIRST_MARKER);
    if disconnect_window_opened {
        tracing::warn!(
            "ACS import re-entered with the disconnect-window marker set — a participant \
             found disconnected stays disconnected and the import continues in its window"
        );
    }
    Ok(WindowMarker {
        disconnect_window_opened,
        opened_before_authorization,
    })
}

/// Refuse to import into a participant that a previous attempt fed part of the
/// ACS to.
async fn refuse_if_quarantined(storage: &SqlitePool, target: &ReplicationTarget) -> Result {
    // A previous attempt fed part of the ACS to Canton before failing. Canton
    // offers no way to ask how much landed, and importing the whole snapshot on
    // top of it is not something the proto sanctions, so refuse rather than
    // guess. Checked before anything else so no export is pulled and nothing is
    // disconnected.
    //
    // Keyed by party and participant, not by run: a fresh add-party would
    // otherwise not see it, and that is the operator's most natural next move.
    if let Some(reason) = storage
        .get_acs_import_quarantine(&target.party_id, &target.target_participant_id)
        .await?
    {
        anyhow::bail!(
            "{participant} is quarantined for {party}: {reason}. Repair the participant \
             (RepairCommitmentsUsingAcs, or restore it), then lift the quarantine with \
             DELETE /acs-import-quarantine before replicating again.",
            participant = target.target_participant_id,
            party = target.party_id
        );
    }
    Ok(())
}

/// Refuse to open the window when this participant lacks a package the
/// party's contracts need.
async fn check_packages_present(config: &NodeConfig, required_package_ids: &[String]) -> Result {
    // Package preflight: refuse to open the disconnect window if this participant
    // is missing any package the party's contracts need. The offline import
    // re-validates every contract (ContractImportMode::Validation) and fails on a
    // missing package — but only AFTER disconnecting, which is the devnet
    // "onboarded a node without the DARs" failure. Catch it here, before any
    // disconnect, with an actionable error and the participant untouched.
    if !required_package_ids.is_empty() {
        let available = local_package_ids(config).await?;
        let missing: Vec<&str> = required_package_ids
            .iter()
            .map(String::as_str)
            .filter(|id| !available.contains(*id))
            .collect();
        if !missing.is_empty() {
            anyhow::bail!(
                "the target participant is missing {n} package(s) required by the party's \
                 contracts — vet the corresponding DAR(s) on it before replicating; the ACS \
                 will not be imported without them. Missing package ids: {missing:?}",
                n = missing.len()
            );
        }
    }
    Ok(())
}

/// What the preflight settled before the window opens.
struct ImportPlan {
    /// The disconnect-window marker found on entry.
    window: WindowMarker,
    /// The logical synchronizer id the contracts sit on.
    synchronizer_id: String,
    /// The party being imported.
    party_id: String,
    /// The admin client that disconnects the synchronizers.
    connectivity: SynchronizerConnectivityServiceClient<Channel>,
    /// The synchronizers to disconnect from.
    connected: Vec<list_connected_synchronizers_response::Result>,
}

/// Resolve the ids the import names, list the connected synchronizers, and
/// check the participant's connection before the window opens.
async fn preflight(
    config: &NodeConfig,
    target: &ReplicationTarget,
    window: WindowMarker,
) -> Result<ImportPlan> {
    let disconnect_window_opened = window.disconnect_window_opened;
    // Logical synchronizer id (see `current_ledger_offset` for the physical-id
    // pitfall) plus the party being imported.
    let synchronizer_id =
        utils::extract_synchronizer_fingerprint(&utils::get_synchronizer_id(config).await?)?;
    let party_id = target.party_id.to_string();

    let mut connectivity =
        SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);

    // Snapshot the currently-connected synchronizers so we can disconnect them
    // one at a time via `DisconnectSynchronizer` rather than the bulk
    // `DisconnectAllSynchronizers` (behaviourally identical in Canton — same
    // connectQueue — but it keeps the bulk call out of our code).
    let mut connected = connectivity
        .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
        .await?
        .into_inner()
        .connected_synchronizers;

    // Preflight. A participant left disconnected by an earlier attempt of this
    // run is already where `ImportPartyAcs` needs it, so the import continues
    // without touching the connection. One that is unhealthy for any other
    // reason is given time to settle, never a `ReconnectSynchronizers`: that
    // replays the ACS journal, and a participant in an unknown state is exactly
    // the one whose replay may be fatal.
    let alias = config.synchronizer();
    if !synchronizer_healthy(&connected, alias) {
        let mut still_in_window = disconnect_window_opened && !connected_to(&connected, alias);
        if still_in_window {
            tokio::time::sleep(REENTRY_GRACE).await;
            connected = connectivity
                .list_connected_synchronizers(tonic::Request::new(
                    ListConnectedSynchronizersRequest {},
                ))
                .await?
                .into_inner()
                .connected_synchronizers;
            still_in_window = !connected_to(&connected, alias);
        }
        if still_in_window {
            tracing::warn!(
                "participant is still disconnected from '{alias}' since the previous attempt — \
                 continuing the import inside the open window"
            );
        } else {
            verify_healthy(config).await.map_err(|e| {
                anyhow::anyhow!(
                    "refusing to start ACS import: participant is not connected and healthy \
                     on synchronizer '{synchronizer}' and did not settle on its own — it is not \
                     reconnected from here; check it before retrying add-party: {e}",
                    synchronizer = config.synchronizer()
                )
            })?;
            connected = connectivity
                .list_connected_synchronizers(tonic::Request::new(
                    ListConnectedSynchronizersRequest {},
                ))
                .await?
                .into_inner()
                .connected_synchronizers;
        }
    }

    Ok(ImportPlan {
        window,
        synchronizer_id,
        party_id,
        connectivity,
        connected,
    })
}

/// Open the window, disconnect, import, and reconnect once. The reconnect
/// runs whatever the import's outcome.
async fn run_in_disconnect_window<F, Fut>(
    config: &NodeConfig,
    storage: &SqlitePool,
    target: &ReplicationTarget,
    plan: ImportPlan,
    source: TransferSource,
    first: PipeBlock,
    next_block: &mut F,
) -> Result
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<PipeBlock>>,
{
    let ImportPlan {
        window,
        synchronizer_id,
        party_id,
        mut connectivity,
        connected,
    } = plan;
    let opened_before_authorization = window.opened_before_authorization;

    // Open the crash-safety window BEFORE disconnecting so a crash between here
    // and a verified reconnect is detected on the next attempt. A window the
    // new member opened before the authorization keeps its own marker.
    target
        .write_artifact_if_absent(storage, target.artifacts.import_inflight, None, b"1")
        .await?;

    tracing::info!(
        "Disconnecting from {count} synchronizer(s) for the ACS import...",
        count = connected.len()
    );
    // Disconnect + import as one fallible unit. Whatever its outcome — including
    // a `DisconnectSynchronizer` failing partway through the loop — the
    // reconnect/health-verify bracket below ALWAYS runs, so the participant is
    // never left (partially) disconnected.
    let import_result = async {
        for s in &connected {
            connectivity
                .disconnect_synchronizer(tonic::Request::new(DisconnectSynchronizerRequest {
                    synchronizer_alias: s.synchronizer_alias.clone(),
                }))
                .await?;
        }
        let params = ImportParams {
            synchronizer_id: &synchronizer_id,
            party_id: &party_id,
            source,
        };
        import_with_restarts(config, storage, target, params, first, next_block).await
    }
    .await;

    // The one reconnect of the step, after the import succeeded or the last
    // attempt failed. A participant left disconnected (or half-reconnected) is
    // a worse failure mode than a failed import, so it runs on both outcomes.
    // Prioritise an unhealthy participant: that's the critical, operator-
    // actionable failure and must not be masked by the import error when both
    // fail. A failed import with a HEALTHY participant is reported after it.
    if let Err(close_err) = close_import_window(config, opened_before_authorization).await {
        let import_note = match &import_result {
            Ok(()) => "the ACS import itself completed".to_string(),
            Err(e) => format!("the ACS import also failed: {e:#}"),
        };
        anyhow::bail!(
            "ACS import did not leave the participant healthy and connected — it may \
             be crash-looping on orphan ACS rows from an unclean shutdown and may need \
             manual repair ({import_note}): {close_err:#}"
        );
    }
    import_result?;

    tracing::info!("ACS snapshot imported successfully");

    Ok(())
}

/// Import the ACS, restarting a broken transfer from block 1 inside the open
/// window, up to the attempt budget of `params.source`.
async fn import_with_restarts<F, Fut>(
    config: &NodeConfig,
    storage: &SqlitePool,
    target: &ReplicationTarget,
    params: ImportParams<'_>,
    first: PipeBlock,
    next_block: &mut F,
) -> Result
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<PipeBlock>>,
{
    let attempts = match params.source {
        TransferSource::Coordinator => acs_import_attempts_in_window(),
        TransferSource::Relay => 1,
    };
    let mut first = first;
    // Bytes that reached Canton over every attempt so far, not just the
    // last one: the quarantine is about what may sit on the participant.
    let mut any_fed = 0u64;
    let mut attempt = 1;
    loop {
        let (fed, cause) =
            match run_import(config, storage, target, params, first, next_block).await {
                Ok(()) => return Ok(()),
                Err(ImportError::Fatal { fed, cause }) => {
                    return Err(give_up_transfer(storage, target, any_fed.max(fed), cause).await);
                }
                Err(ImportError::Transfer { fed, cause }) => (fed, cause),
            };
        any_fed = any_fed.max(fed);
        if attempt >= attempts {
            return Err(give_up_transfer(storage, target, any_fed, cause).await);
        }
        tracing::warn!(
            "ACS transfer attempt {attempt}/{attempts} broke after {fed} bytes: {cause}; \
             restarting it from block 1 without reconnecting"
        );
        tokio::time::sleep(IMPORT_RETRY_DELAY).await;
        first = match pull_first_block(next_block).await {
            Ok(block) => block,
            Err(restart_err) => {
                return Err(give_up_transfer(storage, target, any_fed, cause)
                    .await
                    .context(format!("the transfer could not restart: {restart_err}")));
            }
        };
        attempt += 1;
    }
}

/// True iff `alias` appears in the connected list at all, healthy or not.
fn connected_to(connected: &[list_connected_synchronizers_response::Result], alias: &str) -> bool {
    connected.iter().any(|s| s.synchronizer_alias == alias)
}
