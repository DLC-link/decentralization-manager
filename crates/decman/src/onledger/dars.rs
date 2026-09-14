//! DARs: hash-pinned local upload (design D8).
//!
//! No DAR bytes travel over any decman channel. The proposer pins each file
//! (`filename`, `sha256Hex`, `mainPackageId`, `sizeBytes`) on the
//! `WorkflowProposal`; every operator uploads the same file locally through
//! `POST /dars/upload` with `pin_instance`, and the handler refuses a file
//! whose hash matches no pending pin. The proposer completes the run when
//! every pin is vetted on every participant.
//!
//! The coordination DAR itself is embedded and uploaded by a startup task.
//!
//! Pure helpers are implemented; every participant call is a stub the DARs
//! agent fills in.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use base64::Engine;
use common::{api::DarFile, canton_id::CantonId};
use sha2::{Digest, Sha256};

use crate::config::NodeConfig;

use super::daml::codec::DarPin;

/// The coordination DAR this build ships (design D8).
pub const COORDINATION_DAR_FILENAME: &str = "decman-coordination-v1-0.1.0.dar";

/// The bytes of [`COORDINATION_DAR_FILENAME`].
pub fn embedded_coordination_dar() -> &'static [u8] {
    include_bytes!("../../../../releases/v1/decman-coordination-v1-0.1.0.dar")
}

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

/// Pin decoded DAR files. `main_package_ids` maps a filename to the main
/// package id the local participant reported after upload; a file without
/// one is an error, because the pin must carry it.
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

/// Upload a DAR to the local participant, pinned to
/// `expected_main_package_id`, and vet it. Returns the main package id.
///
/// # Errors
/// Returns an error when the participant rejects the upload or the main
/// package id differs.
pub async fn upload_and_vet_locally(
    _config: &NodeConfig,
    _filename: &str,
    _bytes: &[u8],
    _expected_main_package_id: Option<&str>,
) -> Result<String> {
    bail!("not implemented: local DAR upload with expected_main_package_id (design D8)")
}

/// Which pins each participant has not vetted yet, from
/// `ListVettedPackages(filter_participant)` with the validity window
/// applied. An empty list for every participant means the run is complete.
///
/// # Errors
/// Returns an error when a topology read fails.
pub async fn unvetted_pins_by_participant(
    _config: &NodeConfig,
    _participants: &[CantonId],
    _pins: &[DarPin],
) -> Result<BTreeMap<CantonId, Vec<String>>> {
    bail!("not implemented: vetting observation per participant (design D8)")
}

/// State of the startup coordination-DAR task, shown in `/node-health`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CoordinationDarState {
    pub uploaded: bool,
    pub vetted: bool,
    pub attempts: u32,
    pub last_error: Option<String>,
}

/// Upload and vet the embedded coordination DAR on this participant with
/// retry, when `DECPM_AUTO_UPLOAD_COORDINATION_DAR` is on.
///
/// # Errors
/// Returns an error when the retry budget runs out.
pub async fn ensure_coordination_dar(_config: &NodeConfig) -> Result<CoordinationDarState> {
    bail!("not implemented: startup coordination-DAR upload (design D8)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_dar_is_a_zip_archive() {
        let bytes = embedded_coordination_dar();
        assert!(bytes.len() > 1_000);
        assert_eq!(&bytes[..2], b"PK");
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
}
