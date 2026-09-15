//! ACS synchronization for add-party (design D9): offline replication with
//! an operator-moved file and an on-ledger `AcsManifest`.
//!
//! Every current host captures its export offset as the first action of
//! `CoSignChanges`, before its `Authorize`. When the P2P that marks the
//! joiner `Onboarding` becomes effective, each host exports the snapshot to
//! the spool directory and publishes an `AcsManifest`. The joiner verifies
//! the manifest (rules 1-4 below), imports the file through the existing
//! disconnect / `ImportPartyAcs` / reconnect bracket, and clears its
//! onboarding flag.
//!
//! The bytes Canton streams out of `ExportPartyAcs` are already gzip, so a
//! spool file is those bytes unchanged and `.acs.gz` names it truthfully. No
//! decman node ever sends the file to another node: the operator moves it,
//! or pulls it from `GET /acs-export` and pushes it to `POST /acs-import`.

use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{
        ClearPartyOnboardingFlagRequest, ExportPartyAcsRequest,
        party_management_service_client::PartyManagementServiceClient,
    },
    protocol::v30::PartyToParticipant,
};
use common::{
    canton_id::CantonId,
    types::{WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun},
};
use futures::{Stream, StreamExt, stream::BoxStream};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};

use crate::{
    config::{NodeConfig, Peer},
    consts,
    db::schema::SchemaRead,
    utils,
    workflow::{
        party_replication::{
            ArtifactStore, ReplicationArtifacts, ReplicationTarget,
            acs::{collect_party_package_ids, import_party_acs, open_export_session},
            offset,
            onboarding_flag::has_onboarding_marker,
            pipe::{ExportSession, PipeBlock, PipeTrailer},
        },
        storage::{WorkflowStorage, artifact_kinds},
    },
};

use super::{
    OnLedger,
    daml::{ActiveContract, CoordinationClient, codec::AcsManifestRecord},
    engine::{self, MemberVariant},
    identity::{HostingCheck, verify_hosting},
    now_micros,
    topology::{self, WaitBudget},
};

// ---------------------------------------------------------------------------
// Artifact keys
// ---------------------------------------------------------------------------

/// The artifact kinds of an add-party replication, on both sides. The keys
/// are the legacy ones so a run that started before the upgrade still
/// finds its offsets.
///
/// TODO(workflow::party_replication): this constant belongs next to
/// `ReplicationArtifacts`; the add-party config module that held it is
/// deleted with the 1.x transport code.
pub const ADD_PARTY_ARTIFACTS: ReplicationArtifacts = ReplicationArtifacts {
    export_offset: artifact_kinds::ADD_PARTY_EXPORT_OFFSET,
    pre_activation_offset: artifact_kinds::ADD_PARTY_PRE_ACTIVATION_OFFSET,
    import_inflight: artifact_kinds::ADD_PARTY_ACS_IMPORT_INFLIGHT,
};

/// Written by the import endpoint once `ImportPartyAcs` finished and the
/// participant is healthy again. The joiner's `SyncAcs` step waits for it.
/// Payload: JSON `ImportReport`.
///
/// TODO(workflow::storage::artifact_kinds): move next to the other add-party
/// kinds.
pub const ADD_PARTY_ACS_IMPORTED: &str = "add_party_acs_imported";

/// Bytes pulled from Canton per spool block. Large enough that a terabyte
/// snapshot is a few hundred thousand reads, small enough to hold two in
/// memory.
const SPOOL_BLOCK_BYTES: usize = 4 * 1024 * 1024;

/// Bytes per HTTP response chunk when a spool file is re-exported.
const STREAM_CHUNK_BYTES: usize = 1024 * 1024;

/// An active `AcsManifest` with its contract id.
pub type ManifestContract = ActiveContract<AcsManifestRecord>;

/// A snapshot file in the spool directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpoolFile {
    pub path: PathBuf,
    pub size_bytes: i64,
    pub sha256_hex: String,
    /// Package ids of the contracts in the snapshot; the joiner vets them
    /// before the import.
    pub package_ids: Vec<String>,
}

/// What `import_from_reader` did. Also the JSON payload of
/// [`ADD_PARTY_ACS_IMPORTED`].
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ImportReport {
    pub manifest_cid: String,
    pub exporter_participant: String,
    pub activation_serial: u32,
    pub size_bytes: i64,
    pub sha256_hex: String,
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// The spool directory (`DECPM_ACS_SPOOL_DIR`, default `{data}/acs`).
pub fn spool_dir(config: &NodeConfig) -> PathBuf {
    consts::acs_spool_dir(&config.data_dir())
}

/// The file-name prefix shared by every spool file of `party` for `target`.
fn spool_file_prefix(party: &CantonId, target: &CantonId) -> String {
    format!(
        "{}-{}-{}-",
        party.prefix,
        party.namespace.to_hex(),
        target.prefix
    )
}

/// Where the snapshot of `party` for `target` at `activation_serial` lives.
pub fn spool_path(
    config: &NodeConfig,
    party: &CantonId,
    target: &CantonId,
    activation_serial: u32,
) -> PathBuf {
    spool_dir(config).join(format!(
        "{}{activation_serial}.acs.gz",
        spool_file_prefix(party, target)
    ))
}

/// Where an inbound snapshot is staged on the joiner before the import.
fn import_temp_path(
    config: &NodeConfig,
    party: &CantonId,
    exporter: &CantonId,
    activation_serial: u32,
) -> PathBuf {
    spool_dir(config).join(format!(
        "{}-{}-from-{}-{activation_serial}.import.part",
        party.prefix,
        party.namespace.to_hex(),
        exporter.prefix
    ))
}

fn part_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// Whether a spool-directory entry belongs to `prefix` (a finished file or
/// an abandoned partial write).
fn is_spool_file_for(name: &str, prefix: &str) -> bool {
    name.starts_with(prefix) && (name.ends_with(".acs.gz") || name.ends_with(".acs.gz.part"))
}

// ---------------------------------------------------------------------------
// Run lookup
// ---------------------------------------------------------------------------

/// The replication for `party` moving onto `joiner`, with its artefacts
/// under the run row `instance_name`.
pub fn replication_target(
    party: &CantonId,
    joiner: &CantonId,
    instance_name: String,
) -> ReplicationTarget {
    ReplicationTarget::new(
        party.clone(),
        joiner.clone(),
        instance_name,
        ADD_PARTY_ARTIFACTS,
        ArtifactStore::WorkflowRun,
    )
}

/// Which side of an add-party run a lookup wants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunSide {
    /// The coordinator row or a `Member` peer row: a current host.
    Exporter,
    /// The `Joiner` peer row.
    Joiner,
}

fn new_participant_of(run: &WorkflowRun) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&run.config_json)
        .ok()?
        .get("new_participant_id")?
        .as_str()
        .map(str::to_string)
}

fn matches_side(run: &WorkflowRun, side: RunSide) -> bool {
    let Some(meta) = engine::read_run_meta(run) else {
        return false;
    };
    let is_joiner =
        run.role == WorkflowRole::Peer && meta.member_variant == Some(MemberVariant::Joiner);
    match side {
        RunSide::Joiner => is_joiner,
        RunSide::Exporter => !is_joiner,
    }
}

/// The add-party run of `party` for `joiner` on this node, on `side`. An
/// in-progress row wins; otherwise the newest.
pub fn select_add_party_run<'a>(
    runs: &'a [WorkflowRun],
    party: &CantonId,
    joiner: &CantonId,
    side: RunSide,
) -> Option<&'a WorkflowRun> {
    let joiner_uid = joiner.to_string();
    let mut matches: Vec<&WorkflowRun> = runs
        .iter()
        .filter(|r| r.kind == WorkflowKind::AddParty)
        .filter(|r| r.dec_party_id.as_ref() == Some(party))
        .filter(|r| new_participant_of(r).as_deref() == Some(joiner_uid.as_str()))
        .filter(|r| matches_side(r, side))
        .collect();
    matches.sort_by(|a, b| {
        let live = |r: &WorkflowRun| r.status == WorkflowProgress::InProgress;
        live(b).cmp(&live(a)).then(b.created_at.cmp(&a.created_at))
    });
    matches.first().copied()
}

/// [`select_add_party_run`] over the `workflow_runs` table.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn find_add_party_run(
    db: &SqlitePool,
    party: &CantonId,
    joiner: &CantonId,
    side: RunSide,
) -> Result<Option<WorkflowRun>> {
    let runs = db.get_workflow_runs_by_kind(WorkflowKind::AddParty).await?;
    Ok(select_add_party_run(&runs, party, joiner, side).cloned())
}

// ---------------------------------------------------------------------------
// Offsets
// ---------------------------------------------------------------------------

/// Capture this host's export offset once, before its `Authorize`, and
/// return the offset in force.
///
/// The artefact lives under the run row, and one run exists per (party,
/// joiner, base serial) because the `WorkflowProposal` pins the base serial.
/// A later call keeps the first value: a re-captured offset would land after
/// the activation and `ExportPartyAcs` would search past it.
///
/// # Errors
/// Returns an error when the ledger end cannot be read or the write fails.
pub async fn capture_export_offset(
    db: &SqlitePool,
    config: &NodeConfig,
    run: &WorkflowRun,
    party: &CantonId,
    joiner: &CantonId,
    base_serial: u32,
) -> Result<i64> {
    let target = replication_target(party, joiner, run.instance_name.clone());
    let label = format!("add-party export ({party} -> {joiner}, base serial {base_serial})");
    offset::capture_offset_once(
        config,
        db,
        &target,
        ADD_PARTY_ARTIFACTS.export_offset,
        None,
        None,
        &label,
    )
    .await?;
    offset::persisted_or_derived_offset(
        config,
        db,
        &target,
        ADD_PARTY_ARTIFACTS.export_offset,
        None,
    )
    .await
}

/// Capture the joiner's own pre-activation offset once, keyed by its
/// participant id as `ClearPartyOnboardingFlag` later reads it.
///
/// # Errors
/// Returns an error when the ledger end cannot be read or the write fails.
pub async fn capture_pre_activation_offset(
    db: &SqlitePool,
    config: &NodeConfig,
    run: &WorkflowRun,
    party: &CantonId,
) -> Result<i64> {
    let me = config.participant_id().clone();
    let target = replication_target(party, &me, run.instance_name.clone());
    let self_id = me.to_string();
    offset::capture_offset_once(
        config,
        db,
        &target,
        ADD_PARTY_ARTIFACTS.pre_activation_offset,
        Some(&self_id),
        None,
        "add-party pre-activation",
    )
    .await?;
    offset::persisted_or_derived_offset(
        config,
        db,
        &target,
        ADD_PARTY_ARTIFACTS.pre_activation_offset,
        Some(&self_id),
    )
    .await
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Open `ExportPartyAcs` at an explicit offset.
///
/// TODO(workflow::party_replication::acs): `open_export_session` reads the
/// offset from the artefact store; give it an explicit-offset entry point and
/// delete this copy. The bounded `INVALID_STATE` retry is kept because a
/// re-added participant's old activation is published before the new one.
async fn open_export_at(
    config: &NodeConfig,
    party: &CantonId,
    target: &CantonId,
    begin_offset_exclusive: i64,
) -> Result<ExportSession> {
    const ACTIVATION_TIMEOUT_SECS: i64 = 120;
    let synchronizer_id =
        utils::extract_synchronizer_fingerprint(&utils::get_synchronizer_id(config).await?)?;
    let mut client = PartyManagementServiceClient::new(config.admin_channel().await?);
    let budget = WaitBudget::default();
    for attempt in 1..=budget.max_attempts {
        let request = tonic::Request::new(ExportPartyAcsRequest {
            party_id: party.to_string(),
            synchronizer_id: synchronizer_id.clone(),
            target_participant_uid: target.to_string(),
            begin_offset_exclusive,
            wait_for_activation_timeout: Some(prost_types::Duration {
                seconds: ACTIVATION_TIMEOUT_SECS,
                nanos: 0,
            }),
        });
        match client.export_party_acs(request).await {
            Ok(response) => return Ok(ExportSession::new(response.into_inner())),
            Err(status)
                if status
                    .message()
                    .contains("INVALID_STATE_PARTY_MANAGEMENT_ERROR")
                    && attempt < budget.max_attempts =>
            {
                tracing::warn!(attempt, %status, "ExportPartyAcs not ready; retrying");
                tokio::time::sleep(budget.delay).await;
            }
            Err(status) => return Err(status.into()),
        }
    }
    bail!(
        "ExportPartyAcs of {party} for {target} still not ready after {} attempts",
        budget.max_attempts
    )
}

/// Pull every block of `session` into `path`. The file is written under a
/// `.part` name and renamed at the end, so a file at `path` is always
/// complete. Returns the session's trailer (bytes served and their sha256).
async fn spool_session(mut session: ExportSession, path: &Path) -> Result<PipeTrailer> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create spool directory {}", parent.display()))?;
    }
    let part = part_path(path);
    let result: Result<PipeTrailer> = async {
        let mut file = fs::File::create(&part)
            .await
            .with_context(|| format!("create {}", part.display()))?;
        let mut seq = 1u64;
        let trailer = loop {
            match session.block(seq, SPOOL_BLOCK_BYTES).await? {
                PipeBlock::Data { bytes, .. } => {
                    file.write_all(&bytes).await?;
                    seq += 1;
                }
                PipeBlock::End { trailer, .. } => break trailer,
            }
        };
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&part, path)
            .await
            .with_context(|| format!("rename {} to {}", part.display(), path.display()))?;
        Ok(trailer)
    }
    .await;
    if result.is_err() {
        // A half-written file must not be mistaken for a snapshot later.
        let _ = fs::remove_file(&part).await;
    }
    result
}

/// The package ids the joiner must hold before it imports. Best effort: the
/// read needs a Ledger API credential for the party, which this path does
/// not always have. An empty list makes the joiner skip its preflight; Canton
/// still validates every contract during the import.
async fn snapshot_package_ids(
    config: &NodeConfig,
    party: &CantonId,
    size_bytes: i64,
) -> Vec<String> {
    if size_bytes == 0 {
        return Vec::new();
    }
    match collect_party_package_ids(config, &party.to_string(), None).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(
                %party,
                error = %e,
                "could not collect the party's package ids; the manifest carries none"
            );
            Vec::new()
        }
    }
}

fn spool_file_from(
    path: &Path,
    size_bytes: u64,
    sha256_hex: String,
    package_ids: Vec<String>,
) -> SpoolFile {
    SpoolFile {
        path: path.to_path_buf(),
        size_bytes: i64::try_from(size_bytes).unwrap_or(i64::MAX),
        sha256_hex,
        package_ids,
    }
}

/// Export the party's ACS for `target` from `begin_offset_exclusive` into
/// `path` and describe the file.
///
/// # Errors
/// Returns an error when the export stream fails or the file cannot be
/// written.
pub async fn export_snapshot(
    config: &NodeConfig,
    party: &CantonId,
    target: &CantonId,
    begin_offset_exclusive: i64,
    path: &Path,
) -> Result<SpoolFile> {
    let session = open_export_at(config, party, target, begin_offset_exclusive).await?;
    let trailer = spool_session(session, path).await?;
    let size = i64::try_from(trailer.total_len).unwrap_or(i64::MAX);
    let package_ids = snapshot_package_ids(config, party, size).await;
    tracing::info!(%party, %target, bytes = trailer.total_len, path = %path.display(), "ACS snapshot spooled");
    Ok(spool_file_from(
        path,
        trailer.total_len,
        trailer.sha256,
        package_ids,
    ))
}

/// Export through the existing `open_export_session`, which reads the offset
/// this run captured (or derives one from the activation), into `path`.
///
/// # Errors
/// Returns an error when the offset is unavailable, the export stream fails,
/// or the file cannot be written.
pub async fn export_to_spool(
    config: &NodeConfig,
    db: &SqlitePool,
    replication: &ReplicationTarget,
    path: &Path,
) -> Result<SpoolFile> {
    let session = open_export_session(config, db, replication).await?;
    let trailer = spool_session(session, path).await?;
    let size = i64::try_from(trailer.total_len).unwrap_or(i64::MAX);
    let package_ids = snapshot_package_ids(config, &replication.party_id, size).await;
    tracing::info!(
        party = %replication.party_id,
        target = %replication.target_participant_id,
        bytes = trailer.total_len,
        path = %path.display(),
        "ACS snapshot spooled"
    );
    Ok(spool_file_from(
        path,
        trailer.total_len,
        trailer.sha256,
        package_ids,
    ))
}

/// Reuse a complete spool file at `path` or export a new one. A finished
/// file exists only after the rename in [`spool_session`], so its presence
/// proves the export completed; re-hashing it is far cheaper than a second
/// export when the manifest publish failed after the file was written.
///
/// # Errors
/// As [`export_to_spool`], plus a read error on an existing file.
pub async fn spool_or_export(
    config: &NodeConfig,
    db: &SqlitePool,
    replication: &ReplicationTarget,
    path: &Path,
) -> Result<SpoolFile> {
    if fs::try_exists(path).await? {
        let (size, sha256_hex) = hash_file(path).await?;
        let size_i64 = i64::try_from(size).unwrap_or(i64::MAX);
        let package_ids = snapshot_package_ids(config, &replication.party_id, size_i64).await;
        tracing::info!(path = %path.display(), bytes = size, "reusing the existing spool file");
        return Ok(spool_file_from(path, size, sha256_hex, package_ids));
    }
    export_to_spool(config, db, replication, path).await
}

/// Size and lowercase sha256 of a file.
async fn hash_file(path: &Path) -> Result<(u64, String)> {
    let mut file = fs::File::open(path)
        .await
        .with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, hex::encode(hasher.finalize())))
}

// ---------------------------------------------------------------------------
// Manifests
// ---------------------------------------------------------------------------

/// Publish an `AcsManifest` for a spool file. Returns the contract id.
///
/// # Errors
/// Returns an error when the create fails.
pub async fn publish_manifest(
    client: &CoordinationClient,
    observers: &[CantonId],
    party: &CantonId,
    target: &CantonId,
    activation_serial: u32,
    file: &SpoolFile,
) -> Result<String> {
    let record = AcsManifestRecord {
        exporter: client.node_party().clone(),
        exporter_participant: client.participant_id().to_string(),
        observers: observers.to_vec(),
        dec_party_id: party.to_string(),
        target_participant: target.to_string(),
        activation_serial: i64::from(activation_serial),
        size_bytes: file.size_bytes,
        sha256_hex: file.sha256_hex.clone(),
        package_ids: file.package_ids.clone(),
        exported_at: now_micros(),
    };
    let cid = client.create(&record).await?;
    tracing::info!(%party, %target, activation_serial, cid = %cid, bytes = file.size_bytes, "AcsManifest published");
    Ok(cid)
}

/// Every active `AcsManifest` for `party` visible to this node.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_manifests(
    client: &CoordinationClient,
    party: &CantonId,
) -> Result<Vec<ActiveContract<AcsManifestRecord>>> {
    let party = party.to_string();
    Ok(client
        .list_active::<AcsManifestRecord>()
        .await?
        .into_iter()
        .filter(|m| m.record.dec_party_id == party)
        .collect())
}

/// The manifest `exporter` (a node party) published for `target` at
/// `activation_serial`, if any.
pub fn own_manifest<'a>(
    manifests: &'a [ActiveContract<AcsManifestRecord>],
    exporter: &CantonId,
    target: &CantonId,
    activation_serial: u32,
) -> Option<&'a ActiveContract<AcsManifestRecord>> {
    let target = target.to_string();
    manifests.iter().find(|m| {
        m.record.exporter == *exporter
            && m.record.target_participant == target
            && m.record.activation_serial == i64::from(activation_serial)
    })
}

/// The manifest an exporter participant published for `target` at
/// `activation_serial`, if any.
pub fn manifest_for<'a>(
    manifests: &'a [ActiveContract<AcsManifestRecord>],
    exporter_participant: &CantonId,
    target: &CantonId,
    activation_serial: u32,
) -> Option<&'a ActiveContract<AcsManifestRecord>> {
    let exporter = exporter_participant.to_string();
    let target = target.to_string();
    manifests.iter().find(|m| {
        m.record.exporter_participant == exporter
            && m.record.target_participant == target
            && m.record.activation_serial == i64::from(activation_serial)
    })
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The joiner's four acceptance rules (design D9): (1) the exporter is
/// hosted on `exporterParticipant` with Submission; (2) that participant is a
/// head host with `onboarding == None`; (3) the exporter is the node party
/// the peers table records for it; (4) `activationSerial` is the serial that
/// marked the joiner `Onboarding` and the head still marks it so. These rules
/// also gate the `sizeBytes == 0` fast path.
///
/// The manifest must also name this joiner and this party, and carry a
/// well-formed sha256, so a manifest for another run can never pass.
///
/// # Errors
/// Returns an error naming the first rule that fails.
pub fn verify_manifest(
    manifest: &AcsManifestRecord,
    exporter_hosting: &HostingCheck,
    head_p2p: &PartyToParticipant,
    peers: &[Peer],
    joiner: &CantonId,
    activation_serial: u32,
) -> Result<()> {
    let joiner_uid = joiner.to_string();
    if manifest.target_participant != joiner_uid {
        bail!(
            "manifest targets {}, not this participant {joiner_uid}",
            manifest.target_participant
        );
    }
    if manifest.dec_party_id != head_p2p.party {
        bail!(
            "manifest names party {}, the head mapping is for {}",
            manifest.dec_party_id,
            head_p2p.party
        );
    }
    if manifest.size_bytes < 0 {
        bail!("manifest sizeBytes {} is negative", manifest.size_bytes);
    }
    if !is_sha256_hex(&manifest.sha256_hex) {
        bail!(
            "manifest sha256Hex `{}` is not 64 lowercase hex digits",
            manifest.sha256_hex
        );
    }

    // Rule 1: the signatory really lives on the participant it claims.
    if !exporter_hosting.has_submission() {
        bail!(
            "rule 1: exporter {} is not hosted on {} with Submission ({})",
            manifest.exporter,
            manifest.exporter_participant,
            exporter_hosting.describe()
        );
    }

    // Rule 2: only a fully onboarded host has a complete ACS to export.
    let host = head_p2p
        .participants
        .iter()
        .find(|h| h.participant_uid == manifest.exporter_participant);
    match host {
        None => bail!(
            "rule 2: exporter participant {} does not host {} in the head state",
            manifest.exporter_participant,
            head_p2p.party
        ),
        Some(h) if h.onboarding.is_some() => bail!(
            "rule 2: exporter participant {} is itself still onboarding",
            manifest.exporter_participant
        ),
        Some(_) => {}
    }

    // Rule 3: the operator vouched for this node party when adding the peer.
    let recorded = peers
        .iter()
        .find(|p| p.participant_id.to_string() == manifest.exporter_participant)
        .and_then(|p| p.party.as_ref());
    match recorded {
        None => bail!(
            "rule 3: the peers table records no node party for {}",
            manifest.exporter_participant
        ),
        Some(party) if *party != manifest.exporter => bail!(
            "rule 3: the peers table records {party} for {}, the manifest is signed by {}",
            manifest.exporter_participant,
            manifest.exporter
        ),
        Some(_) => {}
    }

    // Rule 4: the snapshot must belong to this activation, and the flag must
    // still be set or the import would land on a live party.
    if manifest.activation_serial != i64::from(activation_serial) {
        bail!(
            "rule 4: manifest activationSerial {} differs from this run's {activation_serial}",
            manifest.activation_serial
        );
    }
    if !has_onboarding_marker(head_p2p, &joiner_uid) {
        bail!("rule 4: the head PartyToParticipant no longer marks {joiner_uid} as onboarding");
    }
    Ok(())
}

/// What the joiner's `SyncAcs` step should do this tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncDecision {
    /// A verified manifest reports an empty snapshot: skip the import.
    EmptySnapshot { exporter_participant: String },
    /// The import endpoint recorded completion.
    Imported,
    /// This participant holds none of these packages, which the party's
    /// contracts need. The import would fail after the disconnect, so the
    /// run stops here instead (design D9).
    MissingPackages(Vec<String>),
    /// Nothing to act on yet; the text is for the log.
    Waiting(String),
}

/// The package ids a set of verified manifests names, deduplicated.
pub fn manifest_package_ids(verified: &[&AcsManifestRecord]) -> BTreeSet<String> {
    verified
        .iter()
        .flat_map(|m| m.package_ids.iter().cloned())
        .collect()
}

/// The pure decision behind [`empty_fast_path_or_wait`]. `missing` holds the
/// package ids the manifests name that this participant does not hold.
pub fn decide_sync(
    verified: &[&AcsManifestRecord],
    rejected: &[String],
    imported: bool,
    missing: &[String],
) -> SyncDecision {
    if imported {
        return SyncDecision::Imported;
    }
    if let Some(m) = verified.iter().find(|m| m.size_bytes == 0) {
        return SyncDecision::EmptySnapshot {
            exporter_participant: m.exporter_participant.clone(),
        };
    }
    // Before the operator carries a file this node cannot import: the
    // offline import re-validates every contract and would fail only after
    // the disconnect window is open.
    if !missing.is_empty() {
        return SyncDecision::MissingPackages(missing.to_vec());
    }
    if verified.is_empty() {
        return SyncDecision::Waiting(if rejected.is_empty() {
            "no AcsManifest for this activation yet".to_string()
        } else {
            format!("no acceptable AcsManifest yet: {}", rejected.join("; "))
        });
    }
    let exporters: Vec<&str> = verified
        .iter()
        .map(|m| m.exporter_participant.as_str())
        .collect();
    SyncDecision::Waiting(format!(
        "{} verified manifest(s) from {exporters:?}; waiting for the operator to run the import",
        verified.len()
    ))
}

/// What the joiner's `SyncAcs` step asks about.
#[derive(Clone, Copy, Debug)]
pub struct SyncAcsQuery<'a> {
    pub party: &'a CantonId,
    pub joiner: &'a CantonId,
    pub activation_serial: u32,
    /// The joiner's run row, where [`ADD_PARTY_ACS_IMPORTED`] lands.
    pub instance_name: &'a str,
}

/// Read the manifests for this activation, verify each with the D9 rules,
/// and decide: fast path on an empty verified snapshot, done when the import
/// endpoint recorded completion, else wait.
///
/// # Errors
/// Returns an error when a ledger, topology, or database read fails.
pub async fn empty_fast_path_or_wait(
    config: &NodeConfig,
    db: &SqlitePool,
    client: &CoordinationClient,
    sync_id: &str,
    query: &SyncAcsQuery<'_>,
) -> Result<SyncDecision> {
    let SyncAcsQuery {
        party,
        joiner,
        activation_serial,
        instance_name,
    } = *query;
    let imported = db
        .read_artifact(instance_name, ADD_PARTY_ACS_IMPORTED, None)
        .await?
        .is_some();
    if imported {
        return Ok(SyncDecision::Imported);
    }
    let Some(head) = topology::read_accepted_p2p(config, sync_id, party).await? else {
        return Ok(SyncDecision::Waiting(format!(
            "{party} has no accepted PartyToParticipant"
        )));
    };
    let manifests = read_manifests(client, party).await?;
    let peers = db.get_all_peers().await?;
    let joiner_uid = joiner.to_string();
    let mut verified: Vec<&AcsManifestRecord> = Vec::new();
    let mut rejected: Vec<String> = Vec::new();
    for m in manifests
        .iter()
        .filter(|m| m.record.target_participant == joiner_uid)
        .filter(|m| m.record.activation_serial == i64::from(activation_serial))
    {
        let Ok(exporter_participant) = CantonId::parse(&m.record.exporter_participant) else {
            rejected.push(format!(
                "{}: exporterParticipant is not a Canton id",
                m.contract_id
            ));
            continue;
        };
        let hosting = match verify_hosting(config, &m.record.exporter, &exporter_participant).await
        {
            Ok(h) => h,
            Err(e) => {
                rejected.push(format!("{}: hosting check failed: {e}", m.contract_id));
                continue;
            }
        };
        match verify_manifest(
            &m.record,
            &hosting,
            &head.mapping,
            &peers,
            joiner,
            activation_serial,
        ) {
            Ok(()) => verified.push(&m.record),
            Err(e) => rejected.push(format!("{}: {e}", m.contract_id)),
        }
    }
    // The manifests name the packages the party's contracts need. Check
    // them now, while the participant is still connected and nothing has
    // been carried.
    let required = manifest_package_ids(&verified);
    let missing: Vec<String> = if required.is_empty() {
        Vec::new()
    } else {
        let available = crate::workflow::party_replication::acs::local_package_ids(config).await?;
        required
            .into_iter()
            .filter(|id| !available.contains(id))
            .collect()
    };
    Ok(decide_sync(&verified, &rejected, false, &missing))
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// A staged file must be byte-identical to what the manifest describes.
fn check_file_matches(manifest: &AcsManifestRecord, size: u64, sha256_hex: &str) -> Result<()> {
    let expected_size = u64::try_from(manifest.size_bytes).unwrap_or(0);
    if size != expected_size {
        bail!(
            "the file is {size} bytes, the manifest says {}",
            manifest.size_bytes
        );
    }
    if sha256_hex != manifest.sha256_hex {
        bail!(
            "the file hashes to {sha256_hex}, the manifest says {}",
            manifest.sha256_hex
        );
    }
    Ok(())
}

/// Serves a staged file to `import_party_acs` block by block, with a replay
/// of the last block so one transport retry is safe, like `ExportSession`.
struct FileFeeder {
    file: fs::File,
    total_len: u64,
    sha256: String,
    served_seq: u64,
    last: Option<PipeBlock>,
}

impl FileFeeder {
    async fn open(path: &Path, total_len: u64, sha256: String) -> Result<Self> {
        Ok(Self {
            file: fs::File::open(path)
                .await
                .with_context(|| format!("open {}", path.display()))?,
            total_len,
            sha256,
            served_seq: 0,
            last: None,
        })
    }

    async fn next(&mut self, seq: u64) -> Result<PipeBlock> {
        if seq == self.served_seq
            && let Some(last) = &self.last
        {
            return Ok(last.clone());
        }
        if seq != self.served_seq + 1 {
            bail!(
                "ACS spool out of sync: asked for block {seq} after serving {}",
                self.served_seq
            );
        }
        let mut buf = vec![0u8; SPOOL_BLOCK_BYTES];
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = self.file.read(&mut buf[filled..]).await?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        buf.truncate(filled);
        let block = if filled == 0 {
            PipeBlock::End {
                seq,
                trailer: PipeTrailer {
                    total_len: self.total_len,
                    sha256: self.sha256.clone(),
                },
            }
        } else {
            PipeBlock::Data { seq, bytes: buf }
        };
        self.served_seq = seq;
        self.last = Some(block.clone());
        Ok(block)
    }
}

/// Feed a staged, already verified file through the disconnect /
/// `ImportPartyAcs` / reconnect bracket and record completion on the run.
async fn import_file(
    config: &NodeConfig,
    db: &SqlitePool,
    instance_name: &str,
    party: &CantonId,
    manifest: &ActiveContract<AcsManifestRecord>,
    path: &Path,
    activation_serial: u32,
) -> Result<ImportReport> {
    let me = config.participant_id().clone();
    let target = replication_target(party, &me, instance_name.to_string());
    let total_len = u64::try_from(manifest.record.size_bytes).unwrap_or(0);
    let feeder = Arc::new(Mutex::new(
        FileFeeder::open(path, total_len, manifest.record.sha256_hex.clone()).await?,
    ));
    let next_block = move |seq: u64| {
        let feeder = Arc::clone(&feeder);
        async move { feeder.lock().await.next(seq).await }
    };
    import_party_acs(
        config,
        db,
        &target,
        &manifest.record.package_ids,
        next_block,
    )
    .await?;

    let report = ImportReport {
        manifest_cid: manifest.contract_id.clone(),
        exporter_participant: manifest.record.exporter_participant.clone(),
        activation_serial,
        size_bytes: manifest.record.size_bytes,
        sha256_hex: manifest.record.sha256_hex.clone(),
    };
    db.write_artifact(
        instance_name,
        ADD_PARTY_ACS_IMPORTED,
        None,
        &serde_json::to_vec(&report).context("encode import report")?,
    )
    .await?;
    tracing::info!(%party, instance = instance_name, bytes = report.size_bytes, "ACS import recorded");
    Ok(report)
}

/// Import a spool file on the joiner: verify size and hash against the
/// manifest, then run the disconnect / `ImportPartyAcs` / reconnect bracket
/// (which vets every manifest package id locally before it disconnects).
///
/// # Errors
/// Returns an error when a check fails, no joiner run exists, or the import
/// fails.
pub async fn import_snapshot(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
    manifest: &AcsManifestRecord,
    path: &Path,
) -> Result<()> {
    let (size, sha256_hex) = hash_file(path).await?;
    check_file_matches(manifest, size, &sha256_hex)?;
    let me = config.participant_id().clone();
    let run = find_add_party_run(db, party, &me, RunSide::Joiner)
        .await?
        .with_context(|| {
            format!("no add-party run on this node for {party}; accept the invitation first")
        })?;
    let activation_serial = u32::try_from(manifest.activation_serial).unwrap_or(0);
    let active = ActiveContract {
        contract_id: String::new(),
        offset: 0,
        record: manifest.clone(),
    };
    import_file(
        config,
        db,
        &run.instance_name,
        party,
        &active,
        path,
        activation_serial,
    )
    .await?;
    Ok(())
}

/// Everything verified before a single body byte is read.
struct PreparedImport {
    manifest: ActiveContract<AcsManifestRecord>,
    instance_name: String,
    temp: PathBuf,
    // Held until the import finished, so the observer skips this run's
    // ticks instead of racing the disconnect window.
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

/// Match the exporter's manifest, verify it, find the joiner run, and take
/// its lock. Fails before the body is read so a bad request costs nothing.
async fn prepare_import(
    ol: &OnLedger,
    party: &CantonId,
    activation_serial: u32,
    exporter: &CantonId,
) -> Result<PreparedImport> {
    let config = ol.config();
    let db = ol.db();
    let client = ol.client().await?;
    let me = config.participant_id().clone();
    let sync_id = utils::get_synchronizer_id(config).await?;

    let manifests = read_manifests(&client, party).await?;
    let manifest = manifest_for(&manifests, exporter, &me, activation_serial)
        .with_context(|| {
            format!(
                "no AcsManifest from {exporter} for {me} at activation serial {activation_serial}"
            )
        })?
        .clone();
    let head = topology::read_accepted_p2p(config, &sync_id, party)
        .await?
        .with_context(|| format!("{party} has no accepted PartyToParticipant"))?;
    let hosting = verify_hosting(config, &manifest.record.exporter, exporter).await?;
    let peers = db.get_all_peers().await?;
    verify_manifest(
        &manifest.record,
        &hosting,
        &head.mapping,
        &peers,
        &me,
        activation_serial,
    )
    .with_context(|| format!("AcsManifest {} refused", manifest.contract_id))?;

    let run = find_add_party_run(db, party, &me, RunSide::Joiner)
        .await?
        .with_context(|| {
            format!("no add-party run on this node for {party}; accept the invitation first")
        })?;
    if run.status != WorkflowProgress::InProgress {
        bail!(
            "add-party run {} is {}, not in progress",
            run.instance_name,
            run.status
        );
    }
    let guard = ol.run_lock(&run.instance_name).await.lock_owned().await;
    // Under the lock: a second upload must not feed Canton the snapshot
    // again while the flag is still set.
    if let Some(done) = db
        .read_artifact(&run.instance_name, ADD_PARTY_ACS_IMPORTED, None)
        .await?
    {
        let earlier = serde_json::from_slice::<ImportReport>(&done)
            .map(|r| r.manifest_cid)
            .unwrap_or_default();
        bail!(
            "the snapshot of {party} was already imported on this node (manifest {earlier}); \
             the run clears the onboarding flag next"
        );
    }
    let temp = import_temp_path(config, party, exporter, activation_serial);
    if let Some(parent) = temp.parent() {
        fs::create_dir_all(parent).await?;
    }
    Ok(PreparedImport {
        manifest,
        instance_name: run.instance_name,
        temp,
        _guard: guard,
    })
}

/// Compare the staged bytes with the manifest, import, and remove the stage.
async fn finish_import(
    ol: &OnLedger,
    party: &CantonId,
    activation_serial: u32,
    prepared: PreparedImport,
    size: u64,
    sha256_hex: String,
) -> Result<ImportReport> {
    let result = async {
        check_file_matches(&prepared.manifest.record, size, &sha256_hex).with_context(|| {
            format!(
                "uploaded file does not match AcsManifest {}",
                prepared.manifest.contract_id
            )
        })?;
        import_file(
            ol.config(),
            ol.db(),
            &prepared.instance_name,
            party,
            &prepared.manifest,
            &prepared.temp,
            activation_serial,
        )
        .await
    }
    .await;
    // The stage is a copy of the operator's file; nothing resumes from it.
    let _ = fs::remove_file(&prepared.temp).await;
    result
}

/// Write everything `reader` yields into `path`; return size and sha256.
async fn stage_reader<R: AsyncRead + Unpin>(mut reader: R, path: &Path) -> Result<(u64, String)> {
    let mut file = fs::File::create(path)
        .await
        .with_context(|| format!("create {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await?;
        size += n as u64;
    }
    file.flush().await?;
    Ok((size, hex::encode(hasher.finalize())))
}

/// Write everything `stream` yields into `path`; return size and sha256.
async fn stage_stream<S, E>(mut stream: S, path: &Path) -> Result<(u64, String)>
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut file = fs::File::create(path)
        .await
        .with_context(|| format!("create {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow::anyhow!("request body: {e}"))?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        size += chunk.len() as u64;
    }
    file.flush().await?;
    Ok((size, hex::encode(hasher.finalize())))
}

/// `POST /acs-import/{party}?serial=N&exporter=<participant>` (design D9):
/// verify the exporter's manifest first, stage the body into the spool
/// directory while hashing it, compare with the manifest, run the import
/// bracket, and write [`ADD_PARTY_ACS_IMPORTED`] so the joiner's `SyncAcs`
/// step advances.
///
/// # Errors
/// Returns an error when the manifest is missing or refused, no joiner run
/// exists, the body does not match the manifest, or the import fails.
pub async fn import_from_reader<R: AsyncRead + Unpin>(
    ol: &OnLedger,
    party: &CantonId,
    activation_serial: u32,
    exporter: &CantonId,
    reader: R,
) -> Result<ImportReport> {
    let prepared = prepare_import(ol, party, activation_serial, exporter).await?;
    let staged = stage_reader(reader, &prepared.temp).await;
    let (size, sha256_hex) = match staged {
        Ok(v) => v,
        Err(e) => {
            let _ = fs::remove_file(&prepared.temp).await;
            return Err(e);
        }
    };
    finish_import(ol, party, activation_serial, prepared, size, sha256_hex).await
}

/// [`import_from_reader`] for a body that arrives as a byte stream (an
/// actix payload).
///
/// # Errors
/// As [`import_from_reader`].
pub async fn import_from_stream<S, E>(
    ol: &OnLedger,
    party: &CantonId,
    activation_serial: u32,
    exporter: &CantonId,
    body: S,
) -> Result<ImportReport>
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let prepared = prepare_import(ol, party, activation_serial, exporter).await?;
    let staged = stage_stream(body, &prepared.temp).await;
    let (size, sha256_hex) = match staged {
        Ok(v) => v,
        Err(e) => {
            let _ = fs::remove_file(&prepared.temp).await;
            return Err(e);
        }
    };
    finish_import(ol, party, activation_serial, prepared, size, sha256_hex).await
}

// ---------------------------------------------------------------------------
// Export endpoint
// ---------------------------------------------------------------------------

fn file_stream(file: fs::File) -> BoxStream<'static, io::Result<Bytes>> {
    futures::stream::unfold((file, false), |(mut file, done)| async move {
        if done {
            return None;
        }
        let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), (file, false)))
            }
            Err(e) => Some((Err(e), (file, true))),
        }
    })
    .boxed()
}

fn session_stream(session: ExportSession) -> BoxStream<'static, io::Result<Bytes>> {
    futures::stream::unfold(
        (session, 0u64, false),
        |(mut session, seq, done)| async move {
            if done {
                return None;
            }
            let next = seq + 1;
            match session.block(next, SPOOL_BLOCK_BYTES).await {
                Ok(PipeBlock::Data { bytes, .. }) => {
                    Some((Ok(Bytes::from(bytes)), (session, next, false)))
                }
                Ok(PipeBlock::End { .. }) => None,
                Err(e) => Some((Err(io::Error::other(e.to_string())), (session, next, true))),
            }
        },
    )
    .boxed()
}

/// `GET /acs-export/{party}/{target}?serial=N` (design D9): the spool file
/// for that activation when it exists, else a fresh `ExportPartyAcs` stream
/// from this host's captured offset (or one derived from the activation when
/// no run row exists).
///
/// The bytes are Canton's gzip output; the handler sets the content type.
///
/// # Errors
/// Returns an error when the spool file cannot be opened or the export
/// cannot be started. A failure inside the stream surfaces as an `Err` item.
pub async fn export_to_response(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
    target: &CantonId,
    activation_serial: u32,
) -> Result<BoxStream<'static, io::Result<Bytes>>> {
    let path = spool_path(config, party, target, activation_serial);
    if fs::try_exists(&path).await? {
        let file = fs::File::open(&path)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        tracing::info!(path = %path.display(), "serving the spool file");
        return Ok(file_stream(file));
    }
    let session = match find_add_party_run(db, party, target, RunSide::Exporter).await? {
        Some(run) => {
            let replication = replication_target(party, target, run.instance_name);
            open_export_session(config, db, &replication).await?
        }
        None => {
            tracing::warn!(%party, %target, "no add-party run on this node; deriving the export offset from the activation");
            let begin = offset::derive_pre_activation_offset(config, party, target).await?;
            open_export_at(config, party, target, begin).await?
        }
    };
    Ok(session_stream(session))
}

// ---------------------------------------------------------------------------
// Flag clearing and cleanup
// ---------------------------------------------------------------------------

/// `ClearPartyOnboardingFlag` on the joiner from an explicit offset, polled
/// within the default wait budget until `onboarded == true` (design D5: no
/// co-sign round exists for it).
///
/// TODO(workflow::party_replication::onboarding_flag): the shared
/// `request_onboarding_flag_clear` reads the offset from the artefact store;
/// the drivers use that one. This entry point exists for callers that hold
/// the offset already.
///
/// # Errors
/// Returns an error when the flag does not clear within the budget.
pub async fn clear_onboarding_flag(
    config: &NodeConfig,
    party: &CantonId,
    pre_activation_offset: i64,
) -> Result<()> {
    let synchronizer_id =
        utils::extract_synchronizer_fingerprint(&utils::get_synchronizer_id(config).await?)?;
    let mut client = PartyManagementServiceClient::new(config.admin_channel().await?);
    let budget = WaitBudget::default();
    for attempt in 1..=budget.max_attempts {
        let response = client
            .clear_party_onboarding_flag(tonic::Request::new(ClearPartyOnboardingFlagRequest {
                party_id: party.to_string(),
                synchronizer_id: synchronizer_id.clone(),
                begin_offset_exclusive: pre_activation_offset,
                wait_for_activation_timeout: None,
            }))
            .await?
            .into_inner();
        if response.onboarded {
            tracing::info!(%party, attempt, "onboarding flag cleared");
            return Ok(());
        }
        if attempt < budget.max_attempts {
            tokio::time::sleep(budget.delay).await;
        }
    }
    bail!(
        "the onboarding flag of {party} on this participant did not clear within {} attempts",
        budget.max_attempts
    )
}

/// Delete the spool files of `party` for `target` once the joiner is
/// observed `onboarded == true` or the run is dismissed. Returns how many
/// files were removed. A missing spool directory counts as empty.
///
/// # Errors
/// Returns an error when the directory cannot be read or a file cannot be
/// removed.
pub async fn cleanup_spool(
    config: &NodeConfig,
    party: &CantonId,
    target: &CantonId,
) -> Result<usize> {
    let removed = remove_spool_files(&spool_dir(config), &spool_file_prefix(party, target)).await?;
    if removed > 0 {
        tracing::info!(%party, %target, removed, "spool files removed");
    }
    Ok(removed)
}

/// Remove every spool file under `dir` whose name starts with `prefix`.
async fn remove_spool_files(dir: &Path, prefix: &str) -> Result<usize> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };
    let mut removed = 0usize;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_spool_file_for(name, prefix) {
            fs::remove_file(entry.path())
                .await
                .with_context(|| format!("remove {}", entry.path().display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// The distinct package ids of a set of manifests, for callers that show
/// the joiner what to vet.
pub fn package_ids_of(manifests: &[&AcsManifestRecord]) -> BTreeSet<String> {
    manifests
        .iter()
        .flat_map(|m| m.package_ids.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::party_to_participant::{
        HostingParticipant, hosting_participant,
    };
    use common::types::Permission;

    use super::*;

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
    const SHA_EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn id(prefix: &str) -> CantonId {
        CantonId::parse(&format!("{prefix}::{NS}")).expect("id")
    }

    fn host(uid: &str, onboarding: bool) -> HostingParticipant {
        HostingParticipant {
            participant_uid: uid.to_string(),
            permission: 2,
            onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
        }
    }

    fn head(joiner_onboarding: bool) -> PartyToParticipant {
        PartyToParticipant {
            party: id("cbtc").to_string(),
            threshold: 2,
            participants: vec![
                host(&id("participant1").to_string(), false),
                host(&id("participant2").to_string(), false),
                host(&id("participant4").to_string(), joiner_onboarding),
            ],
            party_signing_keys: None,
        }
    }

    fn peer(participant: &str, party: Option<&str>) -> Peer {
        Peer {
            participant_id: id(participant),
            name: participant.to_string(),
            party: party.map(id),
        }
    }

    fn submission() -> HostingCheck {
        HostingCheck {
            mapping_exists: true,
            hosted: true,
            permission: Some(Permission::Submission),
            onboarding: false,
            threshold: 1,
        }
    }

    fn manifest(size: i64) -> AcsManifestRecord {
        AcsManifestRecord {
            exporter: id("node-a"),
            exporter_participant: id("participant1").to_string(),
            observers: vec![id("node-d")],
            dec_party_id: id("cbtc").to_string(),
            target_participant: id("participant4").to_string(),
            activation_serial: 6,
            size_bytes: size,
            sha256_hex: SHA_EMPTY.to_string(),
            package_ids: vec!["pkg-1".into()],
            exported_at: 1,
        }
    }

    fn peers() -> Vec<Peer> {
        vec![
            peer("participant1", Some("node-a")),
            peer("participant2", Some("node-b")),
        ]
    }

    fn verify(m: &AcsManifestRecord) -> Result<()> {
        verify_manifest(
            m,
            &submission(),
            &head(true),
            &peers(),
            &id("participant4"),
            6,
        )
    }

    fn active(record: AcsManifestRecord, cid: &str) -> ActiveContract<AcsManifestRecord> {
        ActiveContract {
            contract_id: cid.into(),
            offset: 1,
            record,
        }
    }

    #[test]
    fn spool_path_names_party_target_and_serial() {
        let config = NodeConfig::default();
        let party = id("cbtc");
        let target = id("participant4");
        let path = spool_path(&config, &party, &target, 7);
        assert!(path.starts_with(spool_dir(&config)));
        let name = path.file_name().and_then(|n| n.to_str()).expect("name");
        assert_eq!(name, format!("cbtc-{NS}-participant4-7.acs.gz"));
        assert!(is_spool_file_for(name, &spool_file_prefix(&party, &target)));
    }

    #[test]
    fn spool_file_matching_is_exact_on_the_target_and_accepts_partials() {
        let prefix = spool_file_prefix(&id("cbtc"), &id("participant1"));
        assert!(is_spool_file_for(
            &format!("cbtc-{NS}-participant1-7.acs.gz"),
            &prefix
        ));
        assert!(is_spool_file_for(
            &format!("cbtc-{NS}-participant1-7.acs.gz.part"),
            &prefix
        ));
        // Another participant whose prefix starts the same way is not ours.
        assert!(!is_spool_file_for(
            &format!("cbtc-{NS}-participant10-7.acs.gz"),
            &prefix
        ));
        assert!(!is_spool_file_for(
            &format!("cbtc-{NS}-participant1-7.import.part"),
            &prefix
        ));
        let temp = import_temp_path(&NodeConfig::default(), &id("cbtc"), &id("participant1"), 7);
        let name = temp.file_name().and_then(|n| n.to_str()).expect("name");
        assert!(
            !is_spool_file_for(name, &prefix),
            "a stage file is never a spool file"
        );
    }

    #[test]
    fn a_manifest_that_passes_every_rule_is_accepted() {
        verify(&manifest(0)).expect("valid");
        verify(&manifest(1234)).expect("valid");
    }

    #[test]
    fn manifest_shape_checks_precede_the_rules() {
        let mut other_target = manifest(1);
        other_target.target_participant = id("participant2").to_string();
        assert!(
            verify(&other_target)
                .unwrap_err()
                .to_string()
                .contains("targets")
        );

        let mut other_party = manifest(1);
        other_party.dec_party_id = id("other").to_string();
        assert!(
            verify(&other_party)
                .unwrap_err()
                .to_string()
                .contains("names party")
        );

        let mut negative = manifest(-1);
        negative.size_bytes = -1;
        assert!(
            verify(&negative)
                .unwrap_err()
                .to_string()
                .contains("negative")
        );

        let mut bad_hash = manifest(1);
        bad_hash.sha256_hex = "ABCD".into();
        assert!(
            verify(&bad_hash)
                .unwrap_err()
                .to_string()
                .contains("sha256Hex")
        );
    }

    #[test]
    fn rule_1_requires_submission_hosting() {
        let confirmation = HostingCheck {
            permission: Some(Permission::Confirmation),
            ..submission()
        };
        let err = verify_manifest(
            &manifest(1),
            &confirmation,
            &head(true),
            &peers(),
            &id("participant4"),
            6,
        )
        .unwrap_err();
        assert!(err.to_string().starts_with("rule 1"), "{err}");
    }

    #[test]
    fn rule_2_requires_a_fully_onboarded_head_host() {
        let mut stranger = manifest(1);
        stranger.exporter_participant = id("participant9").to_string();
        let err = verify(&stranger).unwrap_err();
        assert!(err.to_string().starts_with("rule 2"), "{err}");

        // An exporter that is itself onboarding has no complete ACS.
        let mut p2p = head(true);
        p2p.participants[0].onboarding = Some(hosting_participant::Onboarding {});
        let err = verify_manifest(
            &manifest(1),
            &submission(),
            &p2p,
            &peers(),
            &id("participant4"),
            6,
        )
        .unwrap_err();
        assert!(err.to_string().contains("still onboarding"), "{err}");
    }

    #[test]
    fn rule_3_pins_the_exporter_to_the_peers_table() {
        let unknown = vec![peer("participant2", Some("node-b"))];
        let err = verify_manifest(
            &manifest(1),
            &submission(),
            &head(true),
            &unknown,
            &id("participant4"),
            6,
        )
        .unwrap_err();
        assert!(err.to_string().contains("records no node party"), "{err}");

        let other = vec![peer("participant1", Some("node-z"))];
        let err = verify_manifest(
            &manifest(1),
            &submission(),
            &head(true),
            &other,
            &id("participant4"),
            6,
        )
        .unwrap_err();
        assert!(err.to_string().contains("node-z"), "{err}");

        let no_party = vec![peer("participant1", None)];
        assert!(
            verify_manifest(
                &manifest(1),
                &submission(),
                &head(true),
                &no_party,
                &id("participant4"),
                6
            )
            .is_err()
        );
    }

    #[test]
    fn rule_4_pins_the_activation_serial_and_the_marker() {
        let err = verify_manifest(
            &manifest(1),
            &submission(),
            &head(true),
            &peers(),
            &id("participant4"),
            5,
        )
        .unwrap_err();
        assert!(err.to_string().contains("activationSerial 6"), "{err}");

        let err = verify_manifest(
            &manifest(1),
            &submission(),
            &head(false),
            &peers(),
            &id("participant4"),
            6,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no longer marks"), "{err}");
    }

    #[test]
    fn sync_decision_prefers_completion_then_the_empty_fast_path() {
        let empty = manifest(0);
        let full = manifest(9);
        assert_eq!(
            decide_sync(&[&full], &[], true, &[]),
            SyncDecision::Imported
        );
        assert_eq!(
            decide_sync(&[&full, &empty], &[], false, &[]),
            SyncDecision::EmptySnapshot {
                exporter_participant: id("participant1").to_string()
            }
        );
        assert!(matches!(
            decide_sync(&[&full], &[], false, &[]),
            SyncDecision::Waiting(_)
        ));
        match decide_sync(&[], &["m1: rule 3".into()], false, &[]) {
            SyncDecision::Waiting(why) => assert!(why.contains("rule 3")),
            other => panic!("{other:?}"),
        }
        match decide_sync(&[], &[], false, &[]) {
            SyncDecision::Waiting(why) => assert!(why.contains("no AcsManifest")),
            other => panic!("{other:?}"),
        }
    }

    // The import re-validates every contract and would fail only after the
    // disconnect window opens, so a package this node lacks has to stop the
    // run while it is still connected and nothing has been carried.
    #[test]
    fn a_missing_package_stops_the_joiner_before_the_import() {
        let full = manifest(1);
        assert_eq!(
            decide_sync(&[&full], &[], false, &["pkg-a".to_string()]),
            SyncDecision::MissingPackages(vec!["pkg-a".to_string()])
        );
        // An empty snapshot needs no packages, so it still takes the fast path.
        let mut empty = manifest(1);
        empty.size_bytes = 0;
        assert!(matches!(
            decide_sync(&[&empty], &[], false, &["pkg-a".to_string()]),
            SyncDecision::EmptySnapshot { .. }
        ));
        // A completed import is never undone by a late package check.
        assert_eq!(
            decide_sync(&[&full], &[], true, &["pkg-a".to_string()]),
            SyncDecision::Imported
        );
    }

    #[test]
    fn manifest_package_ids_dedupe_across_manifests() {
        let mut a = manifest(1);
        a.package_ids = vec!["p1".into(), "p2".into()];
        let mut b = manifest(1);
        b.package_ids = vec!["p2".into(), "p3".into()];
        let ids = manifest_package_ids(&[&a, &b]);
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            vec!["p1".to_string(), "p2".to_string(), "p3".to_string()]
        );
    }

    #[test]
    fn manifest_lookups_match_exporter_target_and_serial() {
        let mut from_b = manifest(1);
        from_b.exporter = id("node-b");
        from_b.exporter_participant = id("participant2").to_string();
        let list = vec![active(manifest(1), "m-a"), active(from_b, "m-b")];

        assert_eq!(
            own_manifest(&list, &id("node-b"), &id("participant4"), 6)
                .map(|m| m.contract_id.as_str()),
            Some("m-b")
        );
        assert!(own_manifest(&list, &id("node-b"), &id("participant4"), 7).is_none());
        assert_eq!(
            manifest_for(&list, &id("participant1"), &id("participant4"), 6)
                .map(|m| m.contract_id.as_str()),
            Some("m-a")
        );
        assert!(manifest_for(&list, &id("participant1"), &id("participant2"), 6).is_none());
        assert_eq!(
            package_ids_of(&[&manifest(1)]),
            BTreeSet::from(["pkg-1".to_string()])
        );
    }

    #[test]
    fn file_checks_compare_size_and_hash() {
        let m = manifest(0);
        check_file_matches(&m, 0, SHA_EMPTY).expect("empty file matches");
        assert!(check_file_matches(&m, 1, SHA_EMPTY).is_err());
        assert!(check_file_matches(&m, 0, &"0".repeat(64)).is_err());
    }

    fn run(
        instance: &str,
        role: WorkflowRole,
        variant: Option<MemberVariant>,
        status: WorkflowProgress,
        created_at: i64,
        joiner: &str,
    ) -> WorkflowRun {
        let config_json = serde_json::json!({
            "new_participant_id": id(joiner).to_string(),
        })
        .to_string();
        WorkflowRun {
            instance_name: instance.into(),
            kind: WorkflowKind::AddParty,
            role,
            status,
            current_step: "CoSignChanges".into(),
            step_index: 0,
            step_total: 3,
            config_json,
            coordinator_participant: Some(id("participant1").to_string()),
            coordinator_party: Some(id("node-a")),
            proposal_cid: Some("00p".into()),
            member_variant: variant,
            topology_hashes: Default::default(),
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers: vec![],
            completed_peers: vec![],
            connected_peers: vec![],
            acs_progress: None,
            dec_party_id: Some(id("cbtc")),
            prefix: None,
            participants: vec![],
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            added_participant: None,
            package_names: vec![],
            dar_filenames: vec![],
            error: None,
            dismissed: false,
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn run_selection_separates_sides_and_prefers_live_then_newest() {
        let runs = vec![
            run(
                "old-member",
                WorkflowRole::Peer,
                Some(MemberVariant::Member),
                WorkflowProgress::Failed,
                10,
                "participant4",
            ),
            run(
                "joiner",
                WorkflowRole::Peer,
                Some(MemberVariant::Joiner),
                WorkflowProgress::InProgress,
                20,
                "participant4",
            ),
            run(
                "coordinator",
                WorkflowRole::Coordinator,
                None,
                WorkflowProgress::Completed,
                30,
                "participant4",
            ),
            run(
                "other-joiner",
                WorkflowRole::Peer,
                Some(MemberVariant::Member),
                WorkflowProgress::InProgress,
                40,
                "participant5",
            ),
        ];
        let party = id("cbtc");
        let joiner = id("participant4");

        assert_eq!(
            select_add_party_run(&runs, &party, &joiner, RunSide::Joiner)
                .map(|r| r.instance_name.as_str()),
            Some("joiner")
        );
        // No live exporter row: the newest terminal one still names the offset.
        assert_eq!(
            select_add_party_run(&runs, &party, &joiner, RunSide::Exporter)
                .map(|r| r.instance_name.as_str()),
            Some("coordinator")
        );
        let mut with_live = runs.clone();
        with_live.push(run(
            "live-member",
            WorkflowRole::Peer,
            Some(MemberVariant::Member),
            WorkflowProgress::InProgress,
            5,
            "participant4",
        ));
        assert_eq!(
            select_add_party_run(&with_live, &party, &joiner, RunSide::Exporter)
                .map(|r| r.instance_name.as_str()),
            Some("live-member")
        );
        assert!(select_add_party_run(&runs, &id("other"), &joiner, RunSide::Exporter).is_none());
        assert!(
            select_add_party_run(&runs, &party, &id("participant9"), RunSide::Joiner).is_none()
        );
    }

    #[tokio::test]
    async fn spool_cleanup_removes_only_this_targets_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let config = NodeConfig::default();
        let party = id("cbtc");
        let target = id("participant4");
        let other = id("participant2");
        let spool = dir.path();
        let mine = spool.join(
            spool_path(&config, &party, &target, 6)
                .file_name()
                .expect("name"),
        );
        let mine_part = part_path(&mine);
        let theirs = spool.join(
            spool_path(&config, &party, &other, 6)
                .file_name()
                .expect("name"),
        );
        fs::write(&mine, b"x").await?;
        fs::write(&mine_part, b"y").await?;
        fs::write(&theirs, b"z").await?;

        let prefix = spool_file_prefix(&party, &target);
        assert_eq!(remove_spool_files(spool, &prefix).await?, 2);
        assert!(!fs::try_exists(&mine).await?);
        assert!(!fs::try_exists(&mine_part).await?);
        assert!(fs::try_exists(&theirs).await?);
        // A second pass and a missing directory are both empty, not errors.
        assert_eq!(remove_spool_files(spool, &prefix).await?, 0);
        assert_eq!(remove_spool_files(&spool.join("absent"), &prefix).await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn file_feeder_serves_blocks_then_a_trailer_and_replays_the_last() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("snapshot.acs.gz");
        fs::write(&path, b"hello acs").await?;
        let (size, sha) = hash_file(&path).await?;
        assert_eq!(size, 9);

        let mut feeder = FileFeeder::open(&path, size, sha.clone()).await?;
        let first = feeder.next(1).await?;
        assert_eq!(
            first,
            PipeBlock::Data {
                seq: 1,
                bytes: b"hello acs".to_vec()
            }
        );
        assert_eq!(
            feeder.next(1).await?,
            first,
            "a replay returns the same block"
        );
        assert_eq!(
            feeder.next(2).await?,
            PipeBlock::End {
                seq: 2,
                trailer: PipeTrailer {
                    total_len: 9,
                    sha256: sha
                }
            }
        );
        assert!(feeder.next(5).await.is_err(), "a skip is out of sync");
        Ok(())
    }

    #[tokio::test]
    async fn staging_hashes_what_it_writes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("stage.part");
        let (size, sha) = stage_reader(&b"abc"[..], &path).await?;
        assert_eq!(size, 3);
        assert_eq!(
            sha,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(fs::read(&path).await?, b"abc");

        let chunks = futures::stream::iter(vec![
            Ok::<Bytes, io::Error>(Bytes::from_static(b"a")),
            Ok(Bytes::from_static(b"bc")),
        ]);
        let (size, sha2) = stage_stream(chunks, &path).await?;
        assert_eq!(size, 3);
        assert_eq!(sha2, sha);

        let mut streamed = Vec::new();
        let mut out = file_stream(fs::File::open(&path).await?);
        while let Some(chunk) = out.next().await {
            streamed.extend_from_slice(&chunk?);
        }
        assert_eq!(streamed, b"abc");
        Ok(())
    }
}
