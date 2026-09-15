//! DARs: hash-pinned local upload (design D8).
//!
//! No DAR bytes travel over any decman channel. The proposer pins each file
//! (`filename`, `sha256Hex`, `mainPackageId`, `sizeBytes`) on the
//! `WorkflowProposal`; every operator uploads the same file locally through
//! `POST /dars/upload` with `pin_instance`, and [`upload_pinned`] refuses a
//! file whose hash matches no pin of that run. The proposer completes the
//! run when every pin is vetted on every participant, which it reads from the
//! synchronizer topology store.
//!
//! The main package id of a DAR is computed locally by [`read_main_package_id`]
//! and passed to Canton as `expected_main_package_id`, so two independent
//! checks agree before a package is vetted.
//!
//! The coordination DAR itself is embedded and uploaded by
//! [`startup_upload_coordination_dar`], a background task that retries until
//! the participant is connected. Its progress is readable through
//! [`startup_upload_state`] for `/node-health`.

use std::{
    collections::{BTreeMap, HashSet},
    io::{Cursor, Read},
    sync::{Arc, LazyLock},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    ListConnectedSynchronizersRequest, UploadDarRequest, list_connected_synchronizers_response,
    package_service_client::PackageServiceClient,
    synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
    upload_dar_request::UploadDarData,
};
use common::{api::DarFile, canton_id::CantonId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::{
    config::NodeConfig,
    consts,
    db::{
        rows::{ProposalDecision, ProposalDecisionEntry},
        schema::SchemaRead,
    },
    utils,
};

use super::{
    OnLedger,
    daml::codec::{DarPin, WorkflowKind, WorkflowProposalRecord},
    now_micros, now_secs,
    proposals::ActiveProposal,
    registry,
};

// ---------------------------------------------------------------------------
// The embedded coordination DAR
// ---------------------------------------------------------------------------

/// The coordination DAR this build ships (design D8).
pub const COORDINATION_DAR_FILENAME: &str = "decman-coordination-v1-0.1.0.dar";

/// The main package id of [`COORDINATION_DAR_FILENAME`]. Pinned as a constant
/// so a swapped file fails the upload instead of vetting unknown code; the
/// unit test `embedded_dar_main_package_id_matches_the_pin` keeps the two in
/// step.
pub const COORDINATION_MAIN_PACKAGE_ID: &str =
    "7e35ccdbc0cdc40a020ecc5741c003ba9fa1e349e783057628d8053016fa2c7d";

/// The bytes of [`COORDINATION_DAR_FILENAME`].
pub fn embedded_coordination_dar() -> &'static [u8] {
    include_bytes!("../../../../releases/v1/decman-coordination-v1-0.1.0.dar")
}

// ---------------------------------------------------------------------------
// Hashes and pins
// ---------------------------------------------------------------------------

/// Lowercase hex SHA-256 of a DAR.
pub fn hash_dar(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Decode the base64 `DarFile`s of a request into `(filename, bytes)`.
///
/// # Errors
/// Returns an error naming the file whose content is not valid base64.
pub fn decode_dar_files(files: &[DarFile]) -> Result<Vec<(String, Vec<u8>)>> {
    files
        .iter()
        .map(|f| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&f.data)
                .with_context(|| format!("DAR {} is not valid base64", f.filename))?;
            Ok((f.filename.clone(), bytes))
        })
        .collect()
}

/// The main package id of every decoded file, by filename, from the DAR
/// reader.
///
/// # Errors
/// Returns an error naming the first file that is not a valid DAR.
pub fn main_package_ids(files: &[(String, Vec<u8>)]) -> Result<BTreeMap<String, String>> {
    files
        .iter()
        .map(|(filename, bytes)| {
            let id = read_main_package_id(bytes)
                .with_context(|| format!("DAR {filename} is not a valid DAR"))?;
            Ok((filename.clone(), id))
        })
        .collect()
}

/// Pin decoded DAR files. `main_package_ids` maps a filename to its main
/// package id ([`main_package_ids`]); a file without one is an error,
/// because the pin must carry it.
///
/// # Errors
/// Returns an error for a duplicate filename or a missing main package id.
pub fn pin_dar_files(
    files: &[(String, Vec<u8>)],
    main_package_ids: &BTreeMap<String, String>,
) -> Result<Vec<DarPin>> {
    let mut pins = Vec::with_capacity(files.len());
    let mut seen = std::collections::BTreeSet::new();
    for (filename, bytes) in files {
        if !seen.insert(filename.clone()) {
            bail!("DAR {filename} is listed twice");
        }
        let Some(main_package_id) = main_package_ids.get(filename) else {
            bail!("no main package id for DAR {filename}; upload it locally first");
        };
        pins.push(DarPin {
            filename: filename.clone(),
            sha256_hex: hash_dar(bytes),
            main_package_id: main_package_id.clone(),
            size_bytes: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        });
    }
    Ok(pins)
}

/// The pin an uploaded file matches by hash, if any. The handler refuses a
/// file with no match.
pub fn matching_pin<'a>(pins: &'a [DarPin], bytes: &[u8]) -> Option<&'a DarPin> {
    let hash = hash_dar(bytes);
    pins.iter().find(|p| p.sha256_hex == hash)
}

/// The pin an uploaded file satisfies: hash and size both match (design D8).
///
/// # Errors
/// Returns an error that names the pinned files when the hash matches none,
/// and one that names the pin when the size differs.
pub fn find_pin<'a>(pins: &'a [DarPin], bytes: &[u8]) -> Result<&'a DarPin> {
    let Some(pin) = matching_pin(pins, bytes) else {
        let names: Vec<&str> = pins.iter().map(|p| p.filename.as_str()).collect();
        bail!(
            "sha256 {} matches none of the {} pinned DAR(s): {}",
            hash_dar(bytes),
            pins.len(),
            names.join(", ")
        );
    };
    let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    ensure!(
        pin.size_bytes == size,
        "{}: size {size} differs from the pinned {} bytes",
        pin.filename,
        pin.size_bytes
    );
    Ok(pin)
}

// ---------------------------------------------------------------------------
// DAR reader
// ---------------------------------------------------------------------------

/// The manifest entry that names the main archive of a DAR.
const MANIFEST_PATH: &str = "META-INF/MANIFEST.MF";
const MAIN_DALF_KEY: &str = "Main-Dalf";

/// The main package id of a DAR: the SHA-256 of the main archive's payload,
/// as Canton computes it.
///
/// The DAR is a zip. `META-INF/MANIFEST.MF` names the main `.dalf`; that file
/// is a Daml-LF `Archive` protobuf whose `payload` bytes are hashed. The
/// archive's own `hash` field must agree, so a corrupted or hand-edited
/// archive fails here instead of at the participant.
///
/// # Errors
/// Returns an error when the zip, the manifest, the main archive, or its
/// hash is missing or inconsistent.
pub fn read_main_package_id(bytes: &[u8]) -> Result<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("DAR is not a zip")?;

    let mut manifest = String::new();
    archive
        .by_name(MANIFEST_PATH)
        .with_context(|| format!("DAR has no {MANIFEST_PATH}"))?
        .read_to_string(&mut manifest)
        .context("manifest is not UTF-8")?;
    let entries = parse_manifest(&manifest);
    let Some(main_dalf) = entries.get(MAIN_DALF_KEY) else {
        bail!("manifest has no {MAIN_DALF_KEY} entry");
    };

    let mut dalf = Vec::new();
    archive
        .by_name(main_dalf)
        .with_context(|| format!("main archive {main_dalf} is not in the DAR"))?
        .read_to_end(&mut dalf)
        .context("read the main archive")?;
    main_package_id_of_dalf(&dalf).with_context(|| format!("main archive {main_dalf}"))
}

/// Parse a JAR-style manifest into `key -> value`.
///
/// A value longer than 72 bytes wraps onto the next line with one leading
/// space; the space is a marker, not content, so it is dropped and the rest
/// is appended. Line endings may be CRLF.
pub fn parse_manifest(text: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix(' ') {
            if let Some(value) = current.as_ref().and_then(|k| out.get_mut(k)) {
                value.push_str(rest);
            }
            continue;
        }
        if line.is_empty() {
            current = None;
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            out.insert(key.clone(), value.trim_start().to_string());
            current = Some(key);
        }
    }
    out
}

/// A decoded Daml-LF `Archive`: `hash_function = 1`, `payload = 3`,
/// `hash = 4`. Only these three fields matter; the rest is skipped.
struct LfArchive {
    hash_function: u64,
    payload: Vec<u8>,
    hash: Option<String>,
}

/// The only hash function Daml-LF defines.
const LF_HASH_SHA256: u64 = 0;

/// Protobuf wire types.
const WIRE_VARINT: u64 = 0;
const WIRE_FIXED64: u64 = 1;
const WIRE_LEN: u64 = 2;
const WIRE_FIXED32: u64 = 5;

fn read_varint(bytes: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value: u64 = 0;
    for shift in (0..64).step_by(7) {
        let Some(byte) = bytes.get(*pos) else {
            bail!("truncated varint");
        };
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("varint longer than 10 bytes")
}

fn skip(bytes: &[u8], pos: &mut usize, len: usize) -> Result<()> {
    let end = pos
        .checked_add(len)
        .filter(|end| *end <= bytes.len())
        .context("truncated field")?;
    *pos = end;
    Ok(())
}

/// Decode the three fields of an `Archive` without a generated type: the
/// crate has no Daml-LF protobuf bindings, and the message is three fields.
fn decode_lf_archive(bytes: &[u8]) -> Result<LfArchive> {
    let mut pos = 0;
    let mut hash_function = LF_HASH_SHA256;
    let mut payload = None;
    let mut hash = None;
    while pos < bytes.len() {
        let key = read_varint(bytes, &mut pos)?;
        let field = key >> 3;
        match key & 7 {
            WIRE_VARINT => {
                let value = read_varint(bytes, &mut pos)?;
                if field == 1 {
                    hash_function = value;
                }
            }
            WIRE_FIXED64 => skip(bytes, &mut pos, 8)?,
            WIRE_FIXED32 => skip(bytes, &mut pos, 4)?,
            WIRE_LEN => {
                let len = usize::try_from(read_varint(bytes, &mut pos)?).context("field length")?;
                let start = pos;
                skip(bytes, &mut pos, len)?;
                let slice = &bytes[start..pos];
                match field {
                    3 => payload = Some(slice.to_vec()),
                    4 => {
                        hash =
                            Some(String::from_utf8(slice.to_vec()).context("hash is not UTF-8")?);
                    }
                    _ => {}
                }
            }
            other => bail!("unsupported protobuf wire type {other}"),
        }
    }
    let payload = payload.context("archive has no payload")?;
    Ok(LfArchive {
        hash_function,
        payload,
        hash,
    })
}

/// The package id of one `.dalf`: SHA-256 of the archive payload, checked
/// against the archive's own `hash` field when present.
fn main_package_id_of_dalf(dalf: &[u8]) -> Result<String> {
    let archive = decode_lf_archive(dalf)?;
    ensure!(
        archive.hash_function == LF_HASH_SHA256,
        "archive uses hash function {}, not SHA-256",
        archive.hash_function
    );
    let computed = hex::encode(Sha256::digest(&archive.payload));
    if let Some(declared) = archive.hash {
        ensure!(
            declared.eq_ignore_ascii_case(&computed),
            "archive declares package id {declared} but its payload hashes to {computed}"
        );
    }
    Ok(computed)
}

// ---------------------------------------------------------------------------
// Local upload
// ---------------------------------------------------------------------------

/// The `description` Canton stores for an upload: the filename without its
/// extension, as the legacy upload path did.
fn description_of(filename: &str) -> String {
    filename
        .strip_suffix(".dar")
        .unwrap_or(filename)
        .to_string()
}

/// The logical synchronizer id (`alias::fingerprint`). `UploadDar` parses
/// `synchronizer_id` with `SynchronizerId.fromProtoPrimitive`, which rejects
/// the physical form `alias::fingerprint::protocol` that
/// `utils::get_synchronizer_id` returns.
async fn logical_synchronizer_id(config: &NodeConfig) -> Result<String> {
    let physical = utils::get_synchronizer_id(config).await?;
    utils::extract_synchronizer_fingerprint(&physical)
}

/// Upload a DAR to the local participant, pinned to
/// `expected_main_package_id`, and vet it on the configured synchronizer.
/// Returns the main package id.
///
/// The id is always computed locally first and always sent to Canton as
/// `expected_main_package_id`, so the participant refuses a DAR whose main
/// package differs from what the pin (or this reader) says. A caller that
/// passes `None` still gets the Canton-side check against the local id.
///
/// # Errors
/// Returns an error when the file is not a DAR, when the local id differs
/// from `expected_main_package_id`, or when the participant rejects the
/// upload.
pub async fn upload_and_vet_locally(
    config: &NodeConfig,
    filename: &str,
    bytes: &[u8],
    expected_main_package_id: Option<&str>,
) -> Result<String> {
    let local_id =
        read_main_package_id(bytes).with_context(|| format!("{filename} is not a valid DAR"))?;
    if let Some(expected) = expected_main_package_id {
        ensure!(
            local_id.eq_ignore_ascii_case(expected),
            "{filename}: main package id {local_id} does not match the pinned {expected}"
        );
    }

    let synchronizer_id = logical_synchronizer_id(config).await?;
    let mut client = PackageServiceClient::new(
        config
            .admin_channel()
            .await
            .context("connect to participant Admin API")?,
    );
    let request = UploadDarRequest {
        dars: vec![UploadDarData {
            bytes: bytes.to_vec(),
            description: Some(description_of(filename)),
            expected_main_package_id: Some(local_id.clone()),
        }],
        vet_all_packages: true,
        synchronize_vetting: true,
        synchronizer_id: Some(synchronizer_id),
    };
    tracing::info!(filename, main_package_id = %local_id, "uploading and vetting DAR");
    let response = client
        .upload_dar(tonic::Request::new(request))
        .await
        .with_context(|| format!("UploadDar {filename}"))?
        .into_inner();
    if let Some(reported) = response.dar_ids.first()
        && !reported.eq_ignore_ascii_case(&local_id)
    {
        // Canton accepted the pinned id, so the DAR is what we think it is;
        // a different `dar_ids` entry only means the field is not the main
        // package id on this Canton version.
        tracing::warn!(
            filename,
            reported = %reported,
            local = %local_id,
            "UploadDar reported an unexpected dar id"
        );
    }
    tracing::info!(filename, main_package_id = %local_id, "DAR uploaded and vetted");
    Ok(local_id)
}

// ---------------------------------------------------------------------------
// Vetting observation
// ---------------------------------------------------------------------------

/// The filenames of the pins whose main package id is not in `vetted`.
pub fn missing_pins(pins: &[DarPin], vetted: &HashSet<String>) -> Vec<String> {
    pins.iter()
        .filter(|p| !vetted.contains(&p.main_package_id))
        .map(|p| p.filename.clone())
        .collect()
}

/// Whether every participant in a report has vetted every pin.
pub fn all_vetted(report: &BTreeMap<CantonId, Vec<String>>) -> bool {
    report.values().all(Vec::is_empty)
}

/// One line per participant with something missing, for logs and errors.
pub fn describe_unvetted(report: &BTreeMap<CantonId, Vec<String>>) -> String {
    report
        .iter()
        .filter(|(_, missing)| !missing.is_empty())
        .map(|(participant, missing)| format!("{participant}: {}", missing.join(", ")))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Which pins each participant has not vetted yet, from
/// `ListVettedPackages(filter_participant)` with the validity window
/// applied. Every participant appears; an empty list means it is done.
///
/// # Errors
/// Returns an error when a topology read fails.
pub async fn unvetted_pins_by_participant(
    config: &NodeConfig,
    participants: &[CantonId],
    pins: &[DarPin],
) -> Result<BTreeMap<CantonId, Vec<String>>> {
    let mut report = BTreeMap::new();
    for participant in participants {
        let vetted = registry::fetch_vetted_packages_for(config, participant).await?;
        report.insert(participant.clone(), missing_pins(pins, &vetted));
    }
    Ok(report)
}

/// Which pins this participant has not vetted yet.
///
/// # Errors
/// Returns an error when the topology read fails.
pub async fn unvetted_pins_locally(config: &NodeConfig, pins: &[DarPin]) -> Result<Vec<String>> {
    let vetted = registry::fetch_vetted_packages_for(config, config.participant_id()).await?;
    Ok(missing_pins(pins, &vetted))
}

// ---------------------------------------------------------------------------
// POST /dars/upload with a pin
// ---------------------------------------------------------------------------

/// What [`upload_pinned`] uploaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedUpload {
    pub run_id: String,
    pub proposal_cid: String,
    pub pin: DarPin,
}

/// The active `Dars` proposal that `run_id` (`pin_instance`) names for this
/// node: one this node proposed, or one it was invited to and accepted.
///
/// An invitation that is not accepted yet is refused, because the pins of a
/// proposal the operator has not agreed to must not drive an upload. Two
/// active proposals with the same run id are refused as ambiguous.
///
/// # Errors
/// Returns an error when no proposal matches, when the match is not
/// accepted, or when the match is ambiguous.
pub fn select_pin_proposal<'a>(
    proposals: &'a [ActiveProposal],
    decisions: &[ProposalDecisionEntry],
    me: &CantonId,
    run_id: &str,
    now_micros: i64,
) -> Result<&'a ActiveProposal> {
    let candidates: Vec<&ActiveProposal> = proposals
        .iter()
        .filter(|p| {
            p.record.kind == WorkflowKind::Dars
                && p.record.run_id == run_id
                && p.record.expires_at > now_micros
        })
        .collect();
    if candidates.is_empty() {
        bail!("no active Dars WorkflowProposal has run id {run_id}");
    }
    let invited = candidates.iter().any(|p| p.record.invitees.contains(me));
    let accepted = |cid: &str| {
        decisions
            .iter()
            .any(|d| d.proposal_cid == cid && d.decision == ProposalDecision::Accepted)
    };
    let mut eligible = candidates.into_iter().filter(|p| {
        p.record.proposer == *me || (p.record.invitees.contains(me) && accepted(&p.contract_id))
    });
    let Some(first) = eligible.next() else {
        if invited {
            bail!("run {run_id} invites this node but the invitation is not accepted yet");
        }
        bail!("run {run_id} does not name this node");
    };
    if eligible.next().is_some() {
        bail!("run id {run_id} matches more than one active Dars proposal; cancel the stale one");
    }
    Ok(first)
}

/// Upload a file an operator supplied for a pending pin (design D8): find
/// the run's proposal, match the file to a pin by hash and size, upload it
/// with `expected_main_package_id = pin.mainPackageId`, and vet it. The
/// observer completes the run when it sees the vetting.
///
/// # Errors
/// Returns an error when the run has no accepted proposal for this node,
/// when the file matches no pin, or when the upload fails.
pub async fn upload_pinned(
    ol: &OnLedger,
    pin_instance: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<PinnedUpload> {
    let client = ol.client().await?;
    let proposals = client.list_active::<WorkflowProposalRecord>().await?;
    let decisions = ol.db().get_all_proposal_decisions().await?;
    let proposal = select_pin_proposal(
        &proposals,
        &decisions,
        client.node_party(),
        pin_instance,
        now_micros(),
    )?;
    let pin = find_pin(&proposal.record.dar_pins, bytes)?;
    if filename != pin.filename {
        // The hash decides; the name is a label for the participant.
        tracing::info!(
            uploaded = filename,
            pinned = %pin.filename,
            "uploaded DAR matches a pin under another name"
        );
    }
    upload_and_vet_locally(
        ol.config(),
        &pin.filename,
        bytes,
        Some(&pin.main_package_id),
    )
    .await?;
    Ok(PinnedUpload {
        run_id: proposal.record.run_id.clone(),
        proposal_cid: proposal.contract_id.clone(),
        pin: pin.clone(),
    })
}

// ---------------------------------------------------------------------------
// The startup coordination-DAR task
// ---------------------------------------------------------------------------

/// State of one coordination-DAR attempt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CoordinationDarState {
    pub uploaded: bool,
    pub vetted: bool,
    pub attempts: u32,
    pub last_error: Option<String>,
}

/// Where the startup task is.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupUploadPhase {
    /// The task has not run yet.
    #[default]
    Pending,
    /// `DECPM_AUTO_UPLOAD_COORDINATION_DAR` is off; the operator uploads.
    Disabled,
    /// Trying; `attempts` and `last_error` say how it goes.
    Uploading,
    /// The coordination package is vetted on this participant.
    Ready,
}

/// The startup task's state, as `/node-health` shows it.
///
/// TODO(common/src/coordination.rs): this is a DTO; move it there and export
/// it through `gen-types` when `/node-health` carries it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct StartupUploadState {
    pub phase: StartupUploadPhase,
    pub filename: String,
    pub main_package_id: String,
    pub uploaded: bool,
    pub vetted: bool,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Unix seconds of the last change.
    pub updated_at: i64,
}

impl StartupUploadState {
    fn initial() -> Self {
        Self {
            filename: COORDINATION_DAR_FILENAME.to_string(),
            main_package_id: COORDINATION_MAIN_PACKAGE_ID.to_string(),
            ..Self::default()
        }
    }
}

/// The shared cell the task writes and `/node-health` reads.
pub type StartupUploadCell = Arc<RwLock<StartupUploadState>>;

static STARTUP_UPLOAD: LazyLock<StartupUploadCell> =
    LazyLock::new(|| Arc::new(RwLock::new(StartupUploadState::initial())));

/// The process-wide cell. A handler may hold a clone as a field or call
/// [`startup_upload_state`] directly.
pub fn startup_upload_cell() -> StartupUploadCell {
    STARTUP_UPLOAD.clone()
}

/// A copy of the startup task's current state (`/node-health`).
pub async fn startup_upload_state() -> StartupUploadState {
    STARTUP_UPLOAD.read().await.clone()
}

async fn write_state(cell: &StartupUploadCell, update: impl FnOnce(&mut StartupUploadState)) {
    let mut guard = cell.write().await;
    update(&mut guard);
    guard.updated_at = now_secs();
}

/// First delay after a failed attempt.
const STARTUP_BACKOFF_INITIAL: Duration = Duration::from_secs(2);
/// The delay stops growing here, so a participant that comes back after a
/// long outage is vetted within a minute.
const STARTUP_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Double the delay up to [`STARTUP_BACKOFF_MAX`].
pub fn next_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(STARTUP_BACKOFF_MAX)
}

/// Whether the configured synchronizer is connected and healthy.
///
/// TODO(workflow/party_replication/acs.rs): same rule as its private
/// `synchronizer_healthy`; share it when that file is open for edits.
pub fn is_connected_healthy(
    connected: &[list_connected_synchronizers_response::Result],
    alias: &str,
) -> bool {
    connected
        .iter()
        .any(|s| s.synchronizer_alias == alias && s.healthy)
}

async fn participant_connected(config: &NodeConfig) -> Result<bool> {
    let mut client = SynchronizerConnectivityServiceClient::new(
        config
            .admin_channel()
            .await
            .context("connect to participant Admin API")?,
    );
    let connected = client
        .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
        .await
        .context("ListConnectedSynchronizers")?
        .into_inner()
        .connected_synchronizers;
    Ok(is_connected_healthy(&connected, config.synchronizer()))
}

/// One attempt: upload and vet the embedded coordination DAR on this
/// participant, pinned to [`COORDINATION_MAIN_PACKAGE_ID`].
///
/// Idempotent: a package already vetted is not uploaded again, and Canton
/// treats a re-upload of identical bytes as a no-op. `attempts` is left at
/// zero; the caller that retries counts.
///
/// # Errors
/// Returns an error when a topology read or the upload fails.
pub async fn ensure_coordination_dar(config: &NodeConfig) -> Result<CoordinationDarState> {
    let me = config.participant_id();
    let mut state = CoordinationDarState::default();

    let before = registry::fetch_vetted_packages_for(config, me).await?;
    if before.contains(COORDINATION_MAIN_PACKAGE_ID) {
        state.uploaded = true;
        state.vetted = true;
        return Ok(state);
    }

    upload_and_vet_locally(
        config,
        COORDINATION_DAR_FILENAME,
        embedded_coordination_dar(),
        Some(COORDINATION_MAIN_PACKAGE_ID),
    )
    .await?;
    state.uploaded = true;

    let after = registry::fetch_vetted_packages_for(config, me).await?;
    state.vetted = after.contains(COORDINATION_MAIN_PACKAGE_ID);
    if !state.vetted {
        state.last_error =
            Some("uploaded, but the vetting is not in the synchronizer store yet".to_string());
    }
    Ok(state)
}

/// The startup task body (design D8): retry with backoff until the
/// participant is connected and the coordination package is vetted, and
/// mirror every step into the [`startup_upload_cell`]. Returns when the
/// package is vetted, or at once when `DECPM_AUTO_UPLOAD_COORDINATION_DAR`
/// is off. Never blocks `start_server`; spawn it.
///
/// # Errors
/// Returns an error only when the configuration cannot be read; a failing
/// participant is retried, not reported.
pub async fn startup_upload_coordination_dar(ol: &OnLedger) -> Result<StartupUploadState> {
    let cell = startup_upload_cell();
    if !consts::auto_upload_coordination_dar() {
        write_state(&cell, |s| s.phase = StartupUploadPhase::Disabled).await;
        tracing::info!(
            "DECPM_AUTO_UPLOAD_COORDINATION_DAR is off; upload {COORDINATION_DAR_FILENAME} through POST /dars/upload"
        );
        return Ok(cell.read().await.clone());
    }

    let config = ol.config();
    let mut backoff = STARTUP_BACKOFF_INITIAL;
    let mut attempts: u32 = 0;
    loop {
        attempts = attempts.saturating_add(1);
        write_state(&cell, |s| {
            s.phase = StartupUploadPhase::Uploading;
            s.attempts = attempts;
        })
        .await;

        let outcome = match participant_connected(config).await {
            Ok(true) => ensure_coordination_dar(config).await,
            Ok(false) => Err(anyhow::anyhow!(
                "participant is not connected to synchronizer '{}'",
                config.synchronizer()
            )),
            Err(e) => Err(e),
        };

        let error = match outcome {
            Ok(state) if state.vetted => {
                write_state(&cell, |s| {
                    s.phase = StartupUploadPhase::Ready;
                    s.uploaded = true;
                    s.vetted = true;
                    s.last_error = None;
                })
                .await;
                tracing::info!(
                    attempts,
                    main_package_id = COORDINATION_MAIN_PACKAGE_ID,
                    "coordination DAR is vetted on this participant"
                );
                return Ok(cell.read().await.clone());
            }
            Ok(state) => {
                write_state(&cell, |s| s.uploaded = state.uploaded).await;
                state
                    .last_error
                    .unwrap_or_else(|| "uploaded, but not vetted yet".to_string())
            }
            Err(e) => format!("{e:#}"),
        };
        write_state(&cell, |s| s.last_error = Some(error.clone())).await;
        tracing::warn!(
            attempt = attempts,
            retry_in_secs = backoff.as_secs(),
            error = %error,
            "coordination DAR not ready; retrying"
        );
        tokio::time::sleep(backoff).await;
        backoff = next_backoff(backoff);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::onledger::daml::codec::tests::{party, proposal_full};

    #[test]
    fn embedded_dar_is_a_zip_archive() {
        let bytes = embedded_coordination_dar();
        assert!(bytes.len() > 1_000);
        assert_eq!(&bytes[..2], b"PK");
    }

    #[test]
    fn embedded_dar_main_package_id_matches_the_pin() {
        let id = read_main_package_id(embedded_coordination_dar()).expect("valid DAR");
        assert_eq!(id, COORDINATION_MAIN_PACKAGE_ID);
        assert_eq!(id, id.to_lowercase());
        assert_eq!(id.len(), 64);
    }

    #[test]
    fn hash_is_lowercase_hex_sha256() {
        let h = hash_dar(b"abc");
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn decode_rejects_bad_base64() {
        let files = vec![DarFile {
            filename: "a.dar".into(),
            data: "not base64!".into(),
        }];
        let err = decode_dar_files(&files).expect_err("bad base64");
        assert!(err.to_string().contains("a.dar"));
        let ok = decode_dar_files(&[DarFile {
            filename: "b.dar".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"PK\x03\x04"),
        }])
        .expect("decodes");
        assert_eq!(ok[0].1, b"PK\x03\x04");
    }

    #[test]
    fn pins_carry_hash_size_and_main_package_id() {
        let files = vec![("a.dar".to_string(), b"abc".to_vec())];
        let ids: BTreeMap<String, String> = [("a.dar".to_string(), "pkg-a".to_string())]
            .into_iter()
            .collect();
        let pins = pin_dar_files(&files, &ids).expect("pins");
        assert_eq!(pins[0].filename, "a.dar");
        assert_eq!(pins[0].sha256_hex, hash_dar(b"abc"));
        assert_eq!(pins[0].main_package_id, "pkg-a");
        assert_eq!(pins[0].size_bytes, 3);
        assert_eq!(
            matching_pin(&pins, b"abc").map(|p| p.filename.as_str()),
            Some("a.dar")
        );
        assert!(matching_pin(&pins, b"abd").is_none());
    }

    #[test]
    fn pins_refuse_duplicates_and_missing_ids() {
        let ids: BTreeMap<String, String> = [("a.dar".to_string(), "pkg-a".to_string())]
            .into_iter()
            .collect();
        let dup = vec![
            ("a.dar".to_string(), b"abc".to_vec()),
            ("a.dar".to_string(), b"abd".to_vec()),
        ];
        assert!(pin_dar_files(&dup, &ids).is_err());
        let missing = vec![("b.dar".to_string(), b"abc".to_vec())];
        let err = pin_dar_files(&missing, &ids).expect_err("missing id");
        assert!(err.to_string().contains("upload it locally first"));
    }

    #[test]
    fn main_package_ids_come_from_the_reader() {
        let files = vec![(
            COORDINATION_DAR_FILENAME.to_string(),
            embedded_coordination_dar().to_vec(),
        )];
        let ids = main_package_ids(&files).expect("ids");
        assert_eq!(
            ids.get(COORDINATION_DAR_FILENAME).map(String::as_str),
            Some(COORDINATION_MAIN_PACKAGE_ID)
        );
        let pins = pin_dar_files(&files, &ids).expect("pins");
        assert_eq!(pins[0].main_package_id, COORDINATION_MAIN_PACKAGE_ID);
        assert_eq!(
            pins[0].size_bytes,
            i64::try_from(embedded_coordination_dar().len()).expect("fits")
        );

        let err = main_package_ids(&[("junk.dar".to_string(), b"PK junk".to_vec())])
            .expect_err("not a DAR");
        assert!(err.to_string().contains("junk.dar"), "{err}");
    }

    #[test]
    fn find_pin_checks_hash_then_size() {
        let files = vec![("a.dar".to_string(), b"abc".to_vec())];
        let ids: BTreeMap<String, String> = [("a.dar".to_string(), "pkg-a".to_string())]
            .into_iter()
            .collect();
        let mut pins = pin_dar_files(&files, &ids).expect("pins");

        assert_eq!(find_pin(&pins, b"abc").expect("match").filename, "a.dar");
        let err = find_pin(&pins, b"abd").expect_err("no match");
        assert!(err.to_string().contains("a.dar"), "{err}");

        pins[0].size_bytes = 4;
        let err = find_pin(&pins, b"abc").expect_err("size differs");
        assert!(err.to_string().contains("size 3"), "{err}");
    }

    // -- the DAR reader ------------------------------------------------------

    #[test]
    fn manifest_joins_wrapped_lines_and_drops_the_marker_space() {
        let text = "Manifest-Version: 1.0\r\nMain-Dalf: pkg-abc/pk\r\n g-abc.dalf\r\nDalfs: a.dalf,\r\n  b.dalf\r\nFormat: daml-lf\r\n";
        let entries = parse_manifest(text);
        assert_eq!(
            entries.get("Main-Dalf").map(String::as_str),
            Some("pkg-abc/pkg-abc.dalf")
        );
        // The second space is content: the list separator is ", ".
        assert_eq!(
            entries.get("Dalfs").map(String::as_str),
            Some("a.dalf, b.dalf")
        );
        assert_eq!(entries.get("Format").map(String::as_str), Some("daml-lf"));
        assert!(parse_manifest(" orphan continuation\n").is_empty());
    }

    /// A hand-encoded `Archive`: field 3 (payload) and field 4 (hash) as
    /// length-delimited fields, in protobuf order.
    fn lf_archive(payload: &[u8], hash: Option<&str>) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0x1a); // field 3, wire type 2
        out.push(u8::try_from(payload.len()).expect("short payload"));
        out.extend_from_slice(payload);
        if let Some(hash) = hash {
            out.push(0x22); // field 4, wire type 2
            out.push(u8::try_from(hash.len()).expect("short hash"));
            out.extend_from_slice(hash.as_bytes());
        }
        out
    }

    fn dar_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .expect("start file");
            writer.write_all(bytes).expect("write file");
        }
        writer.finish().expect("finish").into_inner()
    }

    #[test]
    fn archive_package_id_is_the_payload_hash_and_must_match_the_declared_one() {
        let payload = b"payload bytes";
        let expected = hex::encode(Sha256::digest(payload));

        assert_eq!(
            main_package_id_of_dalf(&lf_archive(payload, Some(&expected))).expect("ok"),
            expected
        );
        assert_eq!(
            main_package_id_of_dalf(&lf_archive(payload, None)).expect("no declared hash"),
            expected
        );
        let err = main_package_id_of_dalf(&lf_archive(payload, Some("deadbeef")))
            .expect_err("declared hash differs");
        assert!(err.to_string().contains("deadbeef"), "{err}");
        assert!(
            main_package_id_of_dalf(&[0x1a, 0x05, 0x01]).is_err(),
            "truncated"
        );
        assert!(
            main_package_id_of_dalf(&[0x22, 0x00]).is_err(),
            "no payload"
        );
    }

    #[test]
    fn reader_follows_the_manifest_to_the_main_archive() {
        let payload = b"main payload";
        let id = hex::encode(Sha256::digest(payload));
        let dalf = lf_archive(payload, Some(&id));
        let manifest = format!(
            "Manifest-Version: 1.0\nMain-Dalf: pkg-{id}/pkg-{}\n {}.dalf\nDalfs: other.dalf\n",
            &id[..30],
            &id[30..]
        );
        let main_path = format!("pkg-{id}/pkg-{id}.dalf");
        let entries: Vec<(&str, &[u8])> = vec![
            (MANIFEST_PATH, manifest.as_bytes()),
            (main_path.as_str(), dalf.as_slice()),
            ("pkg/other.dalf", b"not the main one"),
        ];
        let dar = dar_with(&entries);
        assert_eq!(read_main_package_id(&dar).expect("main id"), id);

        let entries: Vec<(&str, &[u8])> = vec![("pkg/a.dalf", dalf.as_slice())];
        let no_manifest = dar_with(&entries);
        let err = read_main_package_id(&no_manifest).expect_err("no manifest");
        assert!(err.to_string().contains(MANIFEST_PATH), "{err}");

        let entries: Vec<(&str, &[u8])> = vec![(MANIFEST_PATH, b"Manifest-Version: 1.0\n")];
        let no_main = dar_with(&entries);
        let err = read_main_package_id(&no_main).expect_err("no Main-Dalf");
        assert!(err.to_string().contains(MAIN_DALF_KEY), "{err}");

        let entries: Vec<(&str, &[u8])> = vec![(MANIFEST_PATH, b"Main-Dalf: missing.dalf\n")];
        let dangling = dar_with(&entries);
        let err = read_main_package_id(&dangling).expect_err("dangling Main-Dalf");
        assert!(err.to_string().contains("missing.dalf"), "{err}");

        assert!(read_main_package_id(b"not a zip").is_err());
    }

    // -- vetting -------------------------------------------------------------

    fn pin(name: &str, id: &str) -> DarPin {
        DarPin {
            filename: name.into(),
            sha256_hex: hash_dar(name.as_bytes()),
            main_package_id: id.into(),
            size_bytes: 1,
        }
    }

    #[test]
    fn missing_pins_are_the_unvetted_main_package_ids() {
        let pins = vec![pin("a.dar", "pkg-a"), pin("b.dar", "pkg-b")];
        let vetted: HashSet<String> = ["pkg-a".to_string()].into_iter().collect();
        assert_eq!(missing_pins(&pins, &vetted), vec!["b.dar".to_string()]);
        assert!(missing_pins(&pins, &["pkg-a".to_string(), "pkg-b".to_string()].into()).is_empty());
        assert!(missing_pins(&[], &HashSet::new()).is_empty());
    }

    #[test]
    fn report_is_complete_only_when_every_list_is_empty() {
        let p1 = party("participant1");
        let p2 = party("participant2");
        let mut report: BTreeMap<CantonId, Vec<String>> = BTreeMap::new();
        report.insert(p1.clone(), vec![]);
        report.insert(p2.clone(), vec!["b.dar".into()]);
        assert!(!all_vetted(&report));
        let text = describe_unvetted(&report);
        assert!(text.contains("participant2"), "{text}");
        assert!(text.contains("b.dar"), "{text}");
        assert!(!text.contains("participant1"), "{text}");

        report.insert(p2, vec![]);
        assert!(all_vetted(&report));
        assert!(describe_unvetted(&report).is_empty());
        assert!(all_vetted(&BTreeMap::new()));
    }

    // -- pin proposal selection ----------------------------------------------

    fn dars_proposal(cid: &str, proposer: &str, run_id: &str) -> ActiveProposal {
        let mut record = proposal_full();
        record.kind = WorkflowKind::Dars;
        record.proposer = party(proposer);
        record.run_id = run_id.into();
        record.invitees = vec![party("node-b"), party("node-c")];
        ActiveProposal {
            contract_id: cid.into(),
            offset: 1,
            record,
        }
    }

    fn decision(cid: &str, decision: ProposalDecision) -> ProposalDecisionEntry {
        ProposalDecisionEntry {
            proposal_cid: cid.into(),
            decision,
            decided_at: 1,
            pinned_hashes: vec![],
        }
    }

    #[test]
    fn pin_proposal_needs_an_accepted_invitation_or_own_authorship() {
        let now = 1_700_000_000_000_000;
        let proposals = vec![dars_proposal("00p", "node-a", "dars-1")];

        // The proposer always qualifies.
        let picked = select_pin_proposal(&proposals, &[], &party("node-a"), "dars-1", now)
            .expect("proposer");
        assert_eq!(picked.contract_id, "00p");

        // An invitee needs the accepted decision.
        let err = select_pin_proposal(&proposals, &[], &party("node-b"), "dars-1", now)
            .expect_err("not accepted");
        assert!(err.to_string().contains("not accepted"), "{err}");
        let declined = [decision("00p", ProposalDecision::Declined)];
        assert!(
            select_pin_proposal(&proposals, &declined, &party("node-b"), "dars-1", now).is_err()
        );
        let accepted = [decision("00p", ProposalDecision::Accepted)];
        assert!(
            select_pin_proposal(&proposals, &accepted, &party("node-b"), "dars-1", now).is_ok()
        );

        // A stranger, a wrong run id, an expired proposal, another kind.
        let err = select_pin_proposal(&proposals, &accepted, &party("node-z"), "dars-1", now)
            .expect_err("not named");
        assert!(err.to_string().contains("does not name"), "{err}");
        assert!(
            select_pin_proposal(&proposals, &accepted, &party("node-a"), "dars-2", now).is_err()
        );
        let expired = proposals[0].record.expires_at;
        assert!(
            select_pin_proposal(&proposals, &accepted, &party("node-a"), "dars-1", expired)
                .is_err()
        );
        let mut other_kind = proposals.clone();
        other_kind[0].record.kind = WorkflowKind::Contracts;
        assert!(
            select_pin_proposal(&other_kind, &accepted, &party("node-a"), "dars-1", now).is_err()
        );
    }

    #[test]
    fn pin_proposal_refuses_an_ambiguous_run_id() {
        let now = 1_700_000_000_000_000;
        let proposals = vec![
            dars_proposal("00p", "node-a", "dars-1"),
            dars_proposal("00q", "node-a", "dars-1"),
        ];
        let err = select_pin_proposal(&proposals, &[], &party("node-a"), "dars-1", now)
            .expect_err("ambiguous");
        assert!(err.to_string().contains("more than one"), "{err}");

        // Two coordinators with the same run id: the accepted one wins.
        let two = vec![
            dars_proposal("00p", "node-a", "dars-1"),
            dars_proposal("00q", "node-d", "dars-1"),
        ];
        let accepted = [decision("00q", ProposalDecision::Accepted)];
        let picked = select_pin_proposal(&two, &accepted, &party("node-b"), "dars-1", now)
            .expect("one accepted");
        assert_eq!(picked.contract_id, "00q");
    }

    // -- startup task --------------------------------------------------------

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        assert_eq!(
            next_backoff(STARTUP_BACKOFF_INITIAL),
            Duration::from_secs(4)
        );
        assert_eq!(next_backoff(Duration::from_secs(40)), STARTUP_BACKOFF_MAX);
        assert_eq!(next_backoff(STARTUP_BACKOFF_MAX), STARTUP_BACKOFF_MAX);
    }

    #[test]
    fn connected_needs_the_alias_present_and_healthy() {
        let entry = |alias: &str, healthy: bool| list_connected_synchronizers_response::Result {
            synchronizer_alias: alias.to_string(),
            healthy,
            ..Default::default()
        };
        assert!(is_connected_healthy(&[entry("global", true)], "global"));
        assert!(!is_connected_healthy(&[entry("global", false)], "global"));
        assert!(!is_connected_healthy(&[entry("other", true)], "global"));
        assert!(!is_connected_healthy(&[], "global"));
    }

    #[test]
    fn startup_state_starts_pending_with_the_pinned_id() {
        let state = StartupUploadState::initial();
        assert_eq!(state.phase, StartupUploadPhase::Pending);
        assert_eq!(state.filename, COORDINATION_DAR_FILENAME);
        assert_eq!(state.main_package_id, COORDINATION_MAIN_PACKAGE_ID);
        assert!(!state.uploaded && !state.vetted);
        let json = serde_json::to_value(&state).expect("json");
        assert_eq!(json["phase"], "pending");
        assert!(json.get("last_error").is_none());
    }

    #[test]
    fn description_strips_the_extension() {
        assert_eq!(description_of("a-1.0.dar"), "a-1.0");
        assert_eq!(description_of("plain"), "plain");
    }
}
