use std::{future::Future, time::Duration};

use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    ContractImportMode, ImportPartyAcsRequest,
    party_management_service_client::PartyManagementServiceClient,
};
use common::types::{AcsTransferDirection, AcsTransferProgress};
use futures::{SinkExt, channel::mpsc};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tonic::Code;

use super::import::TransferSource;
use crate::{
    config::NodeConfig,
    consts::{ACS_BLOCK_FETCH_MAX_DELAY_SECS, acs_block_fetch_attempts},
    db::schema::SchemaWrite,
    error::Result,
    workflow::{
        party_replication::{
            ReplicationTarget, now_ms,
            pipe::{PipeBlock, PipeTrailer},
        },
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// How often a moving transfer records progress for the UI, in bytes. Finer
/// than the log cadence because a readout that only moves every 256 MiB looks
/// stuck: at the rates we see this lands a sample every few seconds, and still
/// only a few dozen writes per gigabyte.
const PROGRESS_ARTIFACT_EVERY_BYTES: u64 = 16 * 1024 * 1024;

/// Record how far a transfer has got, for the UI to read back.
///
/// Best effort by design: this is a readout, and nothing reads it to make a
/// decision, so a failed write warns rather than failing the transfer it is
/// reporting on.
pub async fn record_acs_progress(
    storage: &SqlitePool,
    instance_name: &str,
    direction: AcsTransferDirection,
    bytes: u64,
    block: u64,
    started_at_ms: i64,
) {
    let progress = AcsTransferProgress {
        direction,
        bytes: i64::try_from(bytes).unwrap_or(i64::MAX),
        block: i64::try_from(block).unwrap_or(i64::MAX),
        started_at_ms,
        updated_at_ms: now_ms(),
    };
    let payload = match serde_json::to_vec(&progress) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Could not encode ACS transfer progress: {e}");
            return;
        }
    };
    if let Err(e) = storage
        .write_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_ACS_PROGRESS,
            None,
            &payload,
        )
        .await
    {
        tracing::warn!("Could not record ACS transfer progress: {e}");
    }
}

/// Size of each `ImportPartyAcsRequest` chunk. Stays comfortably below
/// Canton's default 4 MiB gRPC message cap.
const IMPORT_CHUNK_SIZE: usize = 1024 * 1024;

/// Log import progress every this many bytes. The participant is disconnected
/// from every synchronizer for the whole import, so an operator watching a
/// multi-hour transfer needs to see it moving.
const PROGRESS_EVERY_BYTES: u64 = 256 * 1024 * 1024;

/// How many blocks may sit in the import channel ahead of Canton. Bounds the
/// target's memory to a couple of blocks.
const IMPORT_READAHEAD: usize = 2;

/// How many times block 1 is re-requested when a transfer restarts: the source
/// discards its forward-only export on that request and re-opens it on its
/// next step tick, so the first ask normally fails once.
const RESTART_FIRST_BLOCK_ATTEMPTS: u32 = 5;

/// Why one `ImportPartyAcs` attempt did not complete. Both carry how many
/// bytes reached Canton, because that is what decides the quarantine.
pub(super) enum ImportError {
    /// Pulling blocks failed, or Canton answered with a transient status. Worth
    /// another attempt inside the same disconnect window.
    Transfer { fed: u64, cause: anyhow::Error },
    /// Canton rejected the import, or what was fed does not match the export.
    /// Feeding the same bytes again cannot change the answer.
    Fatal { fed: u64, cause: anyhow::Error },
}

impl From<anyhow::Error> for ImportError {
    fn from(cause: anyhow::Error) -> Self {
        Self::Fatal { fed: 0, cause }
    }
}

/// Sort Canton's answer to the whole stream: a status that says "not now" is
/// retried inside the window, anything that judges the content is final.
fn import_rpc_error(status: tonic::Status, fed: u64) -> ImportError {
    let cause = anyhow::Error::from(status).context("ImportPartyAcs did not complete");
    match cause
        .downcast_ref::<tonic::Status>()
        .map(tonic::Status::code)
    {
        Some(Code::Unavailable | Code::DeadlineExceeded | Code::Aborted | Code::Unknown) => {
            ImportError::Transfer { fed, cause }
        }
        _ => ImportError::Fatal { fed, cause },
    }
}

/// Record that a partial ACS may sit on the participant and build the error
/// that ends the step. Nothing is recorded when no byte reached Canton.
pub(super) async fn give_up_transfer(
    storage: &SqlitePool,
    target: &ReplicationTarget,
    fed: u64,
    cause: anyhow::Error,
) -> anyhow::Error {
    if fed == 0 {
        return cause;
    }
    // Cancelling means Canton was never told the snapshot was complete, but it
    // persists contracts batch by batch as they arrive, so it may hold part of
    // the ACS and there is no way to ask. Record that against the participant
    // so any later run refuses, not just a retry of this one.
    let reason = format!(
        "an ACS transfer failed after {fed} bytes had already reached Canton ({cause:#}); \
         the import was cancelled rather than closed, so Canton was not told the \
         snapshot was complete, but this participant may hold part of the ACS"
    );
    if let Err(e) = storage
        .quarantine_acs_import(
            &target.party_id,
            &target.target_participant_id,
            &reason,
            fed,
        )
        .await
    {
        return cause.context(format!(
            "{reason}; recording the quarantine also failed: {e}"
        ));
    }
    cause.context(format!(
        "{reason}. It needs repair (RepairCommitmentsUsingAcs) or a restore before \
         this party is used, and replication is now refused for this participant \
         until the quarantine is lifted"
    ))
}

/// Block 1 of a restarted transfer. The source discards its forward-only
/// export when block 1 is asked for again and re-opens it on its next step
/// tick, so the first request is expected to fail once.
pub(super) async fn pull_first_block<F, Fut>(next_block: &mut F) -> Result<PipeBlock>
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<PipeBlock>>,
{
    let mut last = None;
    for attempt in 1..=RESTART_FIRST_BLOCK_ATTEMPTS {
        match next_block(1).await {
            Ok(block) => return Ok(block),
            Err(e) => {
                tracing::info!(
                    "block 1 not available yet (attempt {attempt}/{RESTART_FIRST_BLOCK_ATTEMPTS}): {e}"
                );
                last = Some(e);
            }
        }
        if attempt < RESTART_FIRST_BLOCK_ATTEMPTS {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("block 1 was never requested")))
}

/// Block `seq`, re-requested with backoff while the source keeps failing to
/// answer. Safe because the source replays the block it served last.
pub(super) async fn fetch_block<F, Fut>(
    next_block: &mut F,
    seq: u64,
    source: TransferSource,
) -> Result<PipeBlock>
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<PipeBlock>>,
{
    let attempts = match source {
        TransferSource::Coordinator => acs_block_fetch_attempts(),
        TransferSource::Relay => 1,
    };
    let max_delay = Duration::from_secs(ACS_BLOCK_FETCH_MAX_DELAY_SECS);
    let mut delay = Duration::from_secs(2);
    let mut last = None;
    for attempt in 1..=attempts {
        match next_block(seq).await {
            Ok(block) => return Ok(block),
            Err(e) => {
                tracing::warn!("ACS block {seq} failed (attempt {attempt}/{attempts}): {e}");
                last = Some(e);
            }
        }
        if attempt < attempts {
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(max_delay);
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("block {seq} was never requested")))
}

/// What one `ImportPartyAcs` attempt is for: the party, the synchronizer its
/// contracts sit on, and where the blocks come from.
#[derive(Clone, Copy)]
pub(super) struct ImportParams<'a> {
    pub(super) synchronizer_id: &'a str,
    pub(super) party_id: &'a str,
    pub(super) source: TransferSource,
}

/// The streamed `ImportPartyAcs` call, isolated so the caller can pair it
/// with the disconnect/reconnect bracket and restart it on a broken transfer.
pub(super) async fn run_import<F, Fut>(
    config: &NodeConfig,
    storage: &SqlitePool,
    target: &ReplicationTarget,
    params: ImportParams<'_>,
    first: PipeBlock,
    next_block: &mut F,
) -> Result<(), ImportError>
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<PipeBlock>>,
{
    tracing::info!("Importing the ACS as it arrives...");

    let mut client = PartyManagementServiceClient::new(config.admin_channel().await?);
    let (mut tx, rx) = mpsc::channel::<ImportPartyAcsRequest>(IMPORT_READAHEAD);

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

    let ImportParams {
        synchronizer_id,
        party_id,
        source,
    } = params;
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
    let mut logged = 0u64;
    let mut recorded = 0u64;
    // A broken transfer restarts from block 1, so anchoring the rate here (not
    // at the run's creation) keeps the reported throughput about this attempt.
    let started_at_ms = now_ms();
    record_acs_progress(
        storage,
        &target.instance_name,
        AcsTransferDirection::Import,
        0,
        0,
        started_at_ms,
    )
    .await;

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
                    if fed / PROGRESS_EVERY_BYTES != logged {
                        logged = fed / PROGRESS_EVERY_BYTES;
                        tracing::info!(
                            "ACS import progress: {mib} MiB fed to Canton (block {seq})",
                            mib = fed / (1024 * 1024)
                        );
                    }
                    if fed / PROGRESS_ARTIFACT_EVERY_BYTES != recorded {
                        recorded = fed / PROGRESS_ARTIFACT_EVERY_BYTES;
                        record_acs_progress(
                            storage,
                            &target.instance_name,
                            AcsTransferDirection::Import,
                            fed,
                            seq,
                            started_at_ms,
                        )
                        .await;
                    }
                }
                PipeBlock::End { trailer, .. } => {
                    // Final sample, so the readout lands on the true total
                    // rather than the last 16 MiB boundary it crossed.
                    record_acs_progress(
                        storage,
                        &target.instance_name,
                        AcsTransferDirection::Import,
                        fed,
                        seq,
                        started_at_ms,
                    )
                    .await;
                    return Ok(Some(trailer));
                }
            }
            seq += 1;
            block = fetch_block(next_block, seq, source).await?;
        }
    }
    .await;

    // How the stream ENDS is a protocol signal, not just cleanup. ImportPartyAcs
    // "assumes the provided snapshot contains the complete and untampered ACS",
    // so closing the sender says "that was all of it". On a fetch failure that
    // would be a lie, and Canton would commit a partial ACS as though it were
    // whole. Cancel the RPC instead, so the stream breaks rather than ending.
    let trailer = match feed {
        Ok(trailer) => {
            tx.close_channel();
            drop(tx);
            let rpc_result = rpc.await.map_err(|e| ImportError::Fatal {
                fed,
                cause: anyhow::Error::from(e).context("the ACS import task did not finish"),
            })?;
            rpc_result.map_err(|status| import_rpc_error(status, fed))?;
            trailer
        }
        Err(cause) => {
            rpc.abort();
            let _ = rpc.await;
            drop(tx);
            return Err(ImportError::Transfer { fed, cause });
        }
    };

    let Some(trailer) = trailer else {
        return Err(ImportError::Fatal {
            fed,
            cause: anyhow::anyhow!("the ACS import stream closed before the export finished"),
        });
    };
    let digest = hex::encode(hasher.finalize());
    if fed != trailer.total_len || digest != trailer.sha256 {
        return Err(ImportError::Fatal {
            fed,
            cause: anyhow::anyhow!(
                "the imported ACS does not match the source: fed {fed} bytes (sha256 \
                 {digest}), source exported {expected} bytes (sha256 {expected_digest}). \
                 The participant now holds an ACS that may be incomplete and needs \
                 repair before the party is used",
                expected = trailer.total_len,
                expected_digest = trailer.sha256
            ),
        });
    }

    tracing::info!("Imported {fed} bytes, digest verified");
    Ok(())
}
