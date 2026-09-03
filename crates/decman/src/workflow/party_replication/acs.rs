use std::{
    collections::{BTreeSet, HashMap, HashSet},
    future::Future,
    time::Duration,
};

use anyhow::Context;
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest, GetLedgerEndRequest,
        WildcardFilter, cumulative_filter, get_active_contracts_response::ContractEntry,
    },
    digitalasset::canton::admin::participant::v30::{
        ContractImportMode, DisconnectSynchronizerRequest, ExportPartyAcsRequest,
        ImportPartyAcsRequest, ListConnectedSynchronizersRequest, ListPackagesRequest,
        ReconnectSynchronizersRequest, list_connected_synchronizers_response,
        package_service_client::PackageServiceClient,
        party_management_service_client::PartyManagementServiceClient,
        synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
    },
};
use futures::SinkExt;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    utils,
    workflow::{
        party_replication::{
            ReplicationTarget,
            pipe::{ExportSession, PipeBlock, PipeTrailer},
        },
        storage::WorkflowStorage,
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

/// How many blocks may sit in the import channel ahead of Canton. Bounds the
/// target's memory to a couple of blocks.
const IMPORT_READAHEAD: usize = 2;

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

    let offset_bytes = storage
        .read_artifact(&target.instance_name, target.artifacts.export_offset, None)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{kind} artifact missing — the pre-topology offset was never captured",
                kind = target.artifacts.export_offset
            )
        })?;
    let begin_offset_exclusive: i64 = String::from_utf8(offset_bytes)?
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse export offset: {e}"))?;

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
///   DecMan or participant crash) knows the participant was left mid-window and
///   recovers it — reconnecting and verifying health — before touching it again;
/// - the participant is never left disconnected: reconnect runs even when the
///   import fails, and success is reported only once the synchronizer is
///   confirmed healthy again (not merely that `ReconnectSynchronizers` was
///   accepted);
/// - a participant that can't be brought back to a healthy connected state
///   yields an actionable error naming the likely orphan-row corruption instead
///   of a cryptic retry-abort.
pub async fn import_party_acs<F, Fut>(
    config: &NodeConfig,
    storage: &SqlitePool,
    target: &ReplicationTarget,
    required_package_ids: &[String],
    mut next_block: F,
) -> Result
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<PipeBlock>>,
{
    // The marker is durable (never cleared), so its presence means the
    // disconnect window was entered on a prior attempt of this run — the
    // participant may have been left disconnected (DecMan died mid-window) or
    // crash-looping (unclean participant shutdown). Recover conservatively:
    // reconnect and verify health before doing anything else. This is a no-op
    // when the participant is already connected and healthy.
    let disconnect_window_opened = storage
        .read_artifact(
            &target.instance_name,
            target.artifacts.import_inflight,
            None,
        )
        .await?
        .is_some();
    if disconnect_window_opened {
        tracing::warn!(
            "ACS import re-entered with the disconnect-window marker set — verifying \
             the participant is reconnected and healthy before retrying"
        );
        reconnect_and_verify_healthy(config).await.map_err(|e| {
            anyhow::anyhow!(
                "participant is not healthy and connected on ACS-import re-entry — it \
                 may be crash-looping on orphan ACS rows left by an unclean shutdown; \
                 the participant needs manual repair before add-party can proceed: {e}"
            )
        })?;
    }

    // Pull the first block before touching the participant. It costs one
    // block of memory and answers the only question that decides whether a
    // disconnect is warranted at all: whether the party has any contracts.
    let first = next_block(1).await?;
    if let PipeBlock::End { trailer, .. } = &first
        && trailer.total_len == 0
    {
        tracing::info!("ACS snapshot is empty — nothing to import");
        return Ok(());
    }

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

    // Preflight: the participant must be healthy and connected before we open
    // the disconnect window. If it's NOT (e.g. already disconnected from a prior
    // interrupted attempt), try to bring it back rather than refusing outright —
    // `ImportPartyAcs` needs it disconnected anyway, but we must first confirm it
    // can reach a healthy connected state, and only THEN re-list the synchronizers
    // to disconnect. Bail only if it genuinely can't be recovered.
    if !synchronizer_healthy(&connected, config.synchronizer()) {
        reconnect_and_verify_healthy(config).await.map_err(|e| {
            anyhow::anyhow!(
                "refusing to start ACS import: participant is not connected and healthy on \
                 synchronizer '{}' and could not be recovered — restore its health before \
                 retrying add-party so the disconnect/import window can't compound a bad \
                 state: {e}",
                config.synchronizer()
            )
        })?;
        connected = connectivity
            .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
            .await?
            .into_inner()
            .connected_synchronizers;
    }

    // Open the crash-safety window BEFORE disconnecting so a crash between here
    // and a verified reconnect is detected on the next attempt.
    storage
        .write_artifact(
            &target.instance_name,
            target.artifacts.import_inflight,
            None,
            b"1",
        )
        .await?;

    tracing::info!(
        "Disconnecting from {} synchronizer(s) for the ACS import...",
        connected.len()
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
        run_import(config, &synchronizer_id, &party_id, first, &mut next_block).await
    }
    .await;

    // ALWAYS reconnect and verify the connection is actually healthy — a
    // participant left disconnected (or half-reconnected) is a worse failure
    // mode than a failed import (which the peer step retries end-to-end).
    let reconnect_result = reconnect_and_verify_healthy(config).await;

    // Prioritise an unhealthy participant: that's the critical, operator-
    // actionable failure and must not be masked by a (retryable) import error
    // when both fail. A failed import with a HEALTHY participant is reported
    // only after, so the peer step can retry it end-to-end.
    if let Err(reconnect_err) = reconnect_result {
        let import_note = match &import_result {
            Ok(()) => "the ACS import itself completed".to_string(),
            Err(e) => format!("the ACS import also failed: {e}"),
        };
        anyhow::bail!(
            "ACS import did not leave the participant healthy and connected — it may \
             be crash-looping on orphan ACS rows from an unclean shutdown and may need \
             manual repair ({import_note}): {reconnect_err}"
        );
    }
    import_result?;

    tracing::info!("ACS snapshot imported successfully");

    Ok(())
}

/// Reconnect the participant to all registered synchronizers and confirm the
/// connection is genuinely healthy. `ReconnectSynchronizers` returning `Ok`
/// only means the request was accepted, not that replay succeeded — so we poll
/// `ListConnectedSynchronizers` until the configured synchronizer reports
/// healthy. A participant that reconnects but then crash-loops (or whose admin
/// API is unreachable) is caught here and surfaced as an error rather than
/// reported as a successful import.
async fn reconnect_and_verify_healthy(config: &NodeConfig) -> Result {
    let mut connectivity =
        SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);
    connectivity
        .reconnect_synchronizers(tonic::Request::new(ReconnectSynchronizersRequest {
            ignore_failures: false,
        }))
        .await?;

    let alias = config.synchronizer();
    let max_attempts = topology_retry_max_attempts();
    let retry_delay = Duration::from_secs(topology_retry_delay_secs());
    // Track the most recent reason we're not yet healthy so the final error
    // carries the actionable root cause (admin API unreachable / RPC error /
    // connected-but-unhealthy) rather than a generic timeout message.
    let mut last_reason = "no successful status poll".to_string();
    for attempt in 1..=max_attempts {
        // Fresh client each poll: a crash-looping participant drops its admin
        // API, so a connect/RPC error is itself the signal recovery has failed.
        let healthy = match config.admin_channel().await {
            Ok(channel) => match SynchronizerConnectivityServiceClient::new(channel)
                .list_connected_synchronizers(tonic::Request::new(
                    ListConnectedSynchronizersRequest {},
                ))
                .await
            {
                Ok(resp) => {
                    let ok =
                        synchronizer_healthy(&resp.into_inner().connected_synchronizers, alias);
                    if !ok {
                        last_reason = format!("'{alias}' not connected or not reporting healthy");
                    }
                    ok
                }
                Err(status) => {
                    tracing::warn!(
                        "ListConnectedSynchronizers failed \
                         (attempt {attempt}/{max_attempts}): {status}"
                    );
                    last_reason = format!("ListConnectedSynchronizers RPC error: {status}");
                    false
                }
            },
            Err(e) => {
                tracing::warn!(
                    "participant admin API unreachable (attempt {attempt}/{max_attempts}): {e}"
                );
                last_reason = format!("admin API unreachable: {e}");
                false
            }
        };
        if healthy {
            return Ok(());
        }
        if attempt < max_attempts {
            tokio::time::sleep(retry_delay).await;
        }
    }
    anyhow::bail!(
        "synchronizer '{alias}' did not become healthy after reconnect within \
         {max_attempts} attempts (last: {last_reason})"
    )
}

/// True iff the participant reports an active, healthy connection to `alias`.
/// The safety-critical predicate behind `reconnect_and_verify_healthy`: a
/// reconnect only counts as successful when the synchronizer is both present in
/// the connected set AND flagged healthy — never on the mere presence of a row
/// or an accepted `ReconnectSynchronizers` request. Reporting a half-reconnected
/// (or crash-looping) participant as success is exactly the failure this fix
/// exists to prevent.
fn synchronizer_healthy(
    connected: &[list_connected_synchronizers_response::Result],
    alias: &str,
) -> bool {
    connected
        .iter()
        .any(|s| s.synchronizer_alias == alias && s.healthy)
}

/// Coordinator side: the distinct package ids referenced by the party's active
/// contracts. Shipped to the target so its ACS-import preflight can verify
/// it has every package the imported contracts need before disconnecting.
pub async fn collect_party_package_ids(
    config: &NodeConfig,
    party_id: &str,
    ledger_token: Option<&str>,
) -> Result<Vec<String>> {
    let mut state = utils::create_state_client(config, ledger_token.map(str::to_string)).await?;
    let ledger_end = state
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::WildcardFilter(
                    WildcardFilter {
                        include_created_event_blob: false,
                    },
                )),
            }],
        },
    );

    let request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            verbose: false,
        }),
        stream_continuation_token: None,
    };

    let mut stream = state
        .get_active_contracts(tonic::Request::new(request))
        .await?
        .into_inner();

    let mut package_ids = BTreeSet::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
            && let Some(template_id) = created.template_id
        {
            package_ids.insert(template_id.package_id);
        }
    }
    Ok(package_ids.into_iter().collect())
}

/// New-member side: package ids currently known to this participant, via the
/// admin `PackageService.ListPackages`. Backs the ACS-import package preflight.
async fn local_package_ids(config: &NodeConfig) -> Result<HashSet<String>> {
    let mut client = PackageServiceClient::new(config.admin_channel().await?);
    let descriptions = client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            // 0 = no limit (the convention used across the codebase); a finite
            // cap could omit a required package on a participant with many
            // packages and wrongly fail the preflight.
            limit: 0,
            filter_name: String::new(),
        }))
        .await?
        .into_inner()
        .package_descriptions;
    Ok(descriptions.into_iter().map(|p| p.package_id).collect())
}

/// The streamed `ImportPartyAcs` call, isolated so the caller can pair it
/// with the disconnect/reconnect bracket.
async fn run_import<F, Fut>(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &str,
    first: PipeBlock,
    next_block: &mut F,
) -> Result
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<PipeBlock>>,
{
    tracing::info!("Importing the ACS as it arrives...");

    let mut client = PartyManagementServiceClient::new(config.admin_channel().await?);
    let (mut tx, rx) = futures::channel::mpsc::channel::<ImportPartyAcsRequest>(IMPORT_READAHEAD);

    // Canton consumes the stream while we are still pulling blocks, so the
    // import and the transfer overlap and neither side accumulates. The RPC
    // runs concurrently rather than being awaited first, because the channel
    // is bounded: awaiting it before feeding would deadlock.
    let rpc = tokio::spawn(async move {
        client
            .import_party_acs(tonic::Request::new(rx))
            .await
            .map(|_| ())
    });

    let synchronizer_id = synchronizer_id.to_string();
    let party_id = party_id.to_string();
    let build = |bytes: Vec<u8>| ImportPartyAcsRequest {
        acs_snapshot: bytes,
        synchronizer_id: Some(synchronizer_id.clone()),
        workflow_id_prefix: Some("add-party-acs-import".to_string()),
        contract_import_mode: Some(ContractImportMode::Validation as i32),
        representative_package_id_override: None,
        party_id: Some(party_id.clone()),
    };

    // Hash and count what we actually feed Canton, so the source's trailer can
    // be checked against it. Nothing is stored, so this is the only evidence
    // that what arrived is what was exported.
    let mut hasher = Sha256::new();
    let mut fed = 0u64;

    // The feeding loop's result is captured rather than propagated with `?`.
    // Returning early here would drop the RPC's JoinHandle, and tokio detaches
    // a dropped handle: the import would keep consuming queued requests while
    // the caller's bracket reconnects the synchronizers, which is exactly the
    // state Canton forbids. Every path below closes the sender and joins the
    // task first.
    let feed: Result<Option<PipeTrailer>> = async {
        let mut block = first;
        let mut seq = 1u64;
        loop {
            match block {
                PipeBlock::Data { bytes, .. } => {
                    hasher.update(&bytes);
                    fed += bytes.len() as u64;
                    // Re-chunk into the import stream: a transport block may be
                    // larger than Canton's inbound gRPC message cap (4 MiB by
                    // default), which the block size deliberately can be, since
                    // it is tuned against the Noise handler timeout instead.
                    for chunk in bytes.chunks(IMPORT_CHUNK_SIZE) {
                        // A closed receiver means the RPC already failed; that
                        // error is the informative one, so stop and let it
                        // surface below.
                        if tx.send(build(chunk.to_vec())).await.is_err() {
                            return Ok(None);
                        }
                    }
                    tracing::debug!("Imported {fed} bytes so far");
                }
                PipeBlock::End { trailer, .. } => return Ok(Some(trailer)),
            }
            seq += 1;
            block = next_block(seq).await?;
        }
    }
    .await;

    // Closing the sender is what tells Canton the snapshot is complete, and on
    // a failure path it is what makes the import terminate instead of waiting
    // for more.
    tx.close_channel();
    drop(tx);

    let rpc_result = rpc.await.context("the ACS import task did not finish")?;

    // Order matters. A fetch failure explains a truncated import, so it is
    // reported first; the RPC error is reported next; only a clean run reaches
    // the trailer check.
    let trailer = feed?;
    rpc_result?;

    let Some(trailer) = trailer else {
        anyhow::bail!("the ACS import stream closed before the export finished");
    };
    let digest = hex::encode(hasher.finalize());
    if fed != trailer.total_len || digest != trailer.sha256 {
        anyhow::bail!(
            "the imported ACS does not match the source: fed {fed} bytes (sha256 \
             {digest}), source exported {expected} bytes (sha256 {expected_digest}). \
             The participant now holds an ACS that may be incomplete and needs \
             repair before the party is used",
            expected = trailer.total_len,
            expected_digest = trailer.sha256
        );
    }

    tracing::info!("Imported {fed} bytes, digest verified");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(alias: &str, healthy: bool) -> list_connected_synchronizers_response::Result {
        list_connected_synchronizers_response::Result {
            synchronizer_alias: alias.to_string(),
            healthy,
            ..Default::default()
        }
    }

    #[test]
    fn healthy_only_when_alias_present_and_flagged_healthy() {
        let alias = "global-domain";

        // Present and healthy → the reconnect is genuinely complete.
        assert!(synchronizer_healthy(&[connected(alias, true)], alias));

        // Present but NOT healthy → reconnect accepted but replay not done yet;
        // must not be reported as success (the crux of the fix).
        assert!(!synchronizer_healthy(&[connected(alias, false)], alias));

        // Absent → participant still disconnected (or connected elsewhere).
        assert!(!synchronizer_healthy(&[connected("other", true)], alias));

        // Empty → fully disconnected / crash-looping.
        assert!(!synchronizer_healthy(&[], alias));

        // The right synchronizer healthy among several → success.
        assert!(synchronizer_healthy(
            &[connected("other", false), connected(alias, true)],
            alias,
        ));
    }
}
