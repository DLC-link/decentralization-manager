//! ACS synchronization for add-party (design D9): offline replication with
//! an operator-moved file and an on-ledger `AcsManifest`.
//!
//! Every current host captures its export offset as the first action of
//! `CoSignChanges`. When the P2P that marks the joiner `Onboarding` becomes
//! effective, each host exports the snapshot to the spool directory and
//! publishes an `AcsManifest`. The joiner verifies the manifest, imports the
//! file through the existing disconnect / `ImportPartyAcs` / reconnect
//! bracket, and clears its onboarding flag.
//!
//! Path helpers and the manifest read are implemented; every export, import,
//! and topology step is a stub the ACS agent fills in.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use canton_proto_rs::com::digitalasset::canton::protocol::v30::PartyToParticipant;
use common::{canton_id::CantonId, types::WorkflowRun};
use sqlx::SqlitePool;

use crate::{
    config::{NodeConfig, Peer},
    consts,
};

use super::{
    daml::{ActiveContract, CoordinationClient, codec::AcsManifestRecord},
    identity::HostingCheck,
};

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

/// The spool directory (`DECPM_ACS_SPOOL_DIR`, default `{data}/acs`).
pub fn spool_dir(config: &NodeConfig) -> PathBuf {
    consts::acs_spool_dir(&config.data_dir())
}

/// Where the snapshot of `party` for `target` at `activation_serial` lives.
pub fn spool_path(
    config: &NodeConfig,
    party: &CantonId,
    target: &CantonId,
    activation_serial: u32,
) -> PathBuf {
    spool_dir(config).join(format!(
        "{}-{}-{}-{activation_serial}.acs.gz",
        party.prefix,
        party.namespace.to_hex(),
        target.prefix
    ))
}

/// Capture this host's export offset once, keyed by party, joiner, and base
/// serial (`capture_offset_once`), before its `Authorize`. Returns the
/// offset in force.
///
/// # Errors
/// Returns an error when the ledger end cannot be read.
pub async fn capture_export_offset(
    _db: &SqlitePool,
    _config: &NodeConfig,
    _run: &WorkflowRun,
    _party: &CantonId,
    _joiner: &CantonId,
    _base_serial: u32,
) -> Result<i64> {
    bail!("not implemented: export offset capture on co-sign (design D9)")
}

/// Export the party's ACS for `target` from `begin_offset_exclusive` into
/// `path`, gzip-compressed, and describe the file.
///
/// # Errors
/// Returns an error when the export stream fails or the file cannot be
/// written.
pub async fn export_snapshot(
    _config: &NodeConfig,
    _party: &CantonId,
    _target: &CantonId,
    _begin_offset_exclusive: i64,
    _path: &Path,
) -> Result<SpoolFile> {
    bail!("not implemented: ExportPartyAcs into the spool directory (design D9)")
}

/// Publish an `AcsManifest` for a spool file. Returns the contract id.
///
/// # Errors
/// Returns an error when the create fails.
pub async fn publish_manifest(
    _client: &CoordinationClient,
    _observers: &[CantonId],
    _party: &CantonId,
    _target: &CantonId,
    _activation_serial: u32,
    _file: &SpoolFile,
) -> Result<String> {
    bail!("not implemented: create AcsManifest (design D9)")
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

/// The joiner's four acceptance rules (design D9): the exporter is hosted
/// on `exporterParticipant` with Submission; that participant is a head host
/// with `onboarding == None`; the exporter is the node party the peers table
/// records for it; `activationSerial` is the earliest serial that marks the
/// joiner `Onboarding` and the head still marks it so. These rules also gate
/// the `sizeBytes == 0` fast path.
///
/// # Errors
/// Returns an error naming the first rule that fails.
pub fn verify_manifest(
    _manifest: &AcsManifestRecord,
    _exporter_hosting: &HostingCheck,
    _head_p2p: &PartyToParticipant,
    _peers: &[Peer],
    _joiner: &CantonId,
    _activation_serial: u32,
) -> Result<()> {
    bail!("not implemented: AcsManifest verification (design D9)")
}

/// Import a spool file on the joiner: verify size and hash against the
/// manifest, verify every package id is vetted locally, then run the
/// disconnect / `ImportPartyAcs` / reconnect bracket.
///
/// # Errors
/// Returns an error when a check fails or the import fails.
pub async fn import_snapshot(
    _config: &NodeConfig,
    _db: &SqlitePool,
    _party: &CantonId,
    _manifest: &AcsManifestRecord,
    _path: &Path,
) -> Result<()> {
    bail!("not implemented: verified ImportPartyAcs (design D9)")
}

/// `ClearPartyOnboardingFlag` on the joiner, polled until `onboarded ==
/// true` (design D5: no co-sign round exists for it).
///
/// # Errors
/// Returns an error when the flag does not clear within the budget.
pub async fn clear_onboarding_flag(
    _config: &NodeConfig,
    _party: &CantonId,
    _pre_activation_offset: i64,
) -> Result<()> {
    bail!("not implemented: ClearPartyOnboardingFlag until onboarded (design D9)")
}

/// Delete the spool files of `party` for `target` once the joiner is
/// observed `onboarded == true` or the run is dismissed. Returns how many
/// files were removed.
///
/// # Errors
/// Returns an error when the directory cannot be read.
pub async fn cleanup_spool(
    _config: &NodeConfig,
    _party: &CantonId,
    _target: &CantonId,
) -> Result<usize> {
    bail!("not implemented: spool cleanup (design D9)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spool_path_names_party_target_and_serial() {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let config = NodeConfig::default();
        let party = CantonId::parse(&format!("cbtc::{ns}")).expect("id");
        let target = CantonId::parse(&format!("participant4::{ns}")).expect("id");
        let path = spool_path(&config, &party, &target, 7);
        assert!(path.starts_with(spool_dir(&config)));
        let name = path.file_name().and_then(|n| n.to_str()).expect("name");
        assert_eq!(name, format!("cbtc-{ns}-participant4-7.acs.gz"));
    }
}
