//! On-disk staging for a replicated ACS snapshot.
//!
//! The snapshot used to be assembled in memory on both sides and shipped whole
//! as one Noise command payload. That put a hard ceiling on it — the chunked
//! transfer's `MAX_CHUNKED_TOTAL_SIZE`, sized for `ListPackages` payloads, not
//! for an ACS — and it meant a transfer that failed at 90% started again from
//! zero.
//!
//! So the snapshot lands in a file and moves in bounded pieces. The source
//! stages once and serves byte ranges out of it; the target appends pieces
//! until it has the whole thing. Neither side holds more than one piece in
//! memory, and either side can say how far it got, which is what makes a
//! resumable transfer possible at all.
//!
//! Deliberately not tus. tus is an upload protocol and the direction is
//! inverted here — our receiver is the client — no embeddable tus server exists
//! for Rust, and it would mean speaking HTTP header semantics inside a bespoke
//! binary frame. What tus is actually worth is its state model: an offset as
//! the protocol primitive. That is a `u64` and a file, not a dependency.

use std::path::{Path, PathBuf};

use anyhow::Context;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::error::Result;

/// Directory name, under the node's data dir, holding staged snapshots.
const STAGING_DIR: &str = "acs-staging";

/// Read buffer for the streaming digest pass.
const DIGEST_BUF_SIZE: usize = 1024 * 1024;

/// Where staged snapshots live.
fn staging_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(STAGING_DIR)
}

/// The file a given replication stages into.
///
/// Named from a hash of the replication's instance name, which both sides
/// already derive from the run, so a resumed transfer finds the same file
/// without either side persisting a handle. Hashing also keeps it a single flat
/// name: an instance name embeds a party id, which contains `::` and on a
/// malformed input could contain path separators.
fn staging_path(data_dir: &Path, instance_name: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(instance_name.as_bytes());
    let digest = hex::encode(hasher.finalize());
    staging_dir(data_dir).join(format!("{digest}.acs"))
}

/// Appends a snapshot to disk while hashing it, so the source never holds more
/// than one export chunk in memory and needs no second pass to describe what it
/// staged.
pub struct StagedWriter {
    path: PathBuf,
    file: tokio::fs::File,
    hasher: Sha256,
    len: u64,
}

impl StagedWriter {
    /// Truncate this replication's staging file and open it for writing.
    ///
    /// # Errors
    /// Returns an error if the staging directory or file cannot be created.
    pub async fn create(data_dir: &Path, instance_name: &str) -> Result<Self> {
        let dir = staging_dir(data_dir);
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("Failed to create ACS staging dir {}", dir.display()))?;
        let path = staging_path(data_dir, instance_name);
        let file = tokio::fs::File::create(&path)
            .await
            .with_context(|| format!("Failed to create ACS staging file {}", path.display()))?;
        Ok(Self {
            path,
            file,
            hasher: Sha256::new(),
            len: 0,
        })
    }

    /// Append `bytes` to the staged snapshot.
    ///
    /// # Errors
    /// Returns an error if the write fails.
    pub async fn write(&mut self, bytes: &[u8]) -> Result {
        self.file
            .write_all(bytes)
            .await
            .with_context(|| format!("Failed to write ACS staging {}", self.path.display()))?;
        self.hasher.update(bytes);
        self.len += bytes.len() as u64;
        Ok(())
    }

    /// Bytes staged so far.
    pub fn staged_bytes(&self) -> u64 {
        self.len
    }

    /// Flush and return the snapshot's length and hex SHA-256.
    ///
    /// # Errors
    /// Returns an error if the flush fails.
    pub async fn finish(mut self) -> Result<(u64, String)> {
        self.file
            .flush()
            .await
            .with_context(|| format!("Failed to flush ACS staging {}", self.path.display()))?;
        Ok((self.len, hex::encode(self.hasher.finalize())))
    }
}

/// Bytes already staged for this replication, or `None` when nothing is staged.
///
/// This is the resume point: the target asks itself this before requesting a
/// range, so an interrupted transfer continues instead of restarting.
///
/// # Errors
/// Returns an error if the file exists but cannot be inspected.
pub async fn staged_len(data_dir: &Path, instance_name: &str) -> Result<Option<u64>> {
    let path = staging_path(data_dir, instance_name);
    match tokio::fs::metadata(&path).await {
        Ok(meta) => Ok(Some(meta.len())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to stat {}", path.display())),
    }
}

/// Read up to `len` bytes of the staged snapshot from `offset`.
///
/// A short read means the end of the snapshot was reached, which is how the
/// target learns it is done without the source tracking per-peer progress.
///
/// # Errors
/// Returns an error if nothing is staged, or if `offset` is past the end.
pub async fn read_range(
    data_dir: &Path,
    instance_name: &str,
    offset: u64,
    len: usize,
) -> Result<Vec<u8>> {
    let path = staging_path(data_dir, instance_name);
    let mut file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("Failed to open staged ACS {}", path.display()))?;
    let total = file
        .metadata()
        .await
        .with_context(|| format!("Failed to stat staged ACS {}", path.display()))?
        .len();
    if offset > total {
        anyhow::bail!("ACS range offset {offset} is past the staged length {total}");
    }
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .with_context(|| format!("Failed to seek staged ACS to {offset}"))?;

    let remaining = usize::try_from(total - offset).unwrap_or(usize::MAX);
    let mut buf = vec![0u8; len.min(remaining)];
    file.read_exact(&mut buf)
        .await
        .with_context(|| format!("Failed to read {} bytes at {offset}", buf.len()))?;
    Ok(buf)
}

/// Append `bytes` at `offset`, returning the new staged length.
///
/// `offset` must equal the current length. Rejecting anything else is what
/// keeps a retried or reordered range from corrupting the snapshot: a duplicate
/// piece is refused rather than appended twice.
///
/// # Errors
/// Returns an error if `offset` does not match the current staged length, or if
/// the write fails.
pub async fn append(
    data_dir: &Path,
    instance_name: &str,
    offset: u64,
    bytes: &[u8],
) -> Result<u64> {
    let dir = staging_dir(data_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("Failed to create ACS staging dir {}", dir.display()))?;
    let path = staging_path(data_dir, instance_name);

    let current = staged_len(data_dir, instance_name).await?.unwrap_or(0);
    if offset != current {
        anyhow::bail!(
            "ACS range arrived at offset {offset} but {current} bytes are staged — \
             refusing to append out of order"
        );
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .with_context(|| format!("Failed to open ACS staging {}", path.display()))?;
    file.write_all(bytes)
        .await
        .with_context(|| format!("Failed to append to ACS staging {}", path.display()))?;
    file.flush()
        .await
        .with_context(|| format!("Failed to flush ACS staging {}", path.display()))?;
    Ok(current + bytes.len() as u64)
}

/// Hex SHA-256 of the staged snapshot, read in a streaming pass.
///
/// The target verifies the assembled snapshot against the source's manifest
/// before importing it. It cannot carry an incremental hasher across a restart,
/// so it hashes once at the end instead.
///
/// # Errors
/// Returns an error if nothing is staged or the read fails.
pub async fn digest(data_dir: &Path, instance_name: &str) -> Result<String> {
    let path = staging_path(data_dir, instance_name);
    let mut file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("Failed to open staged ACS {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; DIGEST_BUF_SIZE];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .with_context(|| format!("Failed to read staged ACS {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Path of the staged snapshot, for the import's streaming read.
pub fn staged_file(data_dir: &Path, instance_name: &str) -> PathBuf {
    staging_path(data_dir, instance_name)
}

/// Delete this replication's staged snapshot if present.
///
/// # Errors
/// Returns an error if the file exists but cannot be removed.
pub async fn discard(data_dir: &Path, instance_name: &str) -> Result {
    let path = staging_path(data_dir, instance_name);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: &str = "cbtc-network::1220ab-add-party-1717000000";

    fn tmp() -> Result<tempfile::TempDir> {
        tempfile::tempdir().context("Failed to create temp dir")
    }

    /// Nothing staged must be distinguishable from a staged empty snapshot:
    /// the first is "start from zero", the second is "there is nothing to send".
    #[tokio::test]
    async fn staged_len_is_none_before_anything_is_written() -> Result {
        let dir = tmp()?;
        assert_eq!(staged_len(dir.path(), RUN).await?, None);

        let writer = StagedWriter::create(dir.path(), RUN).await?;
        let (len, _) = writer.finish().await?;
        assert_eq!(len, 0);
        assert_eq!(staged_len(dir.path(), RUN).await?, Some(0));
        Ok(())
    }

    /// The writer's reported length and digest must match a fresh streaming
    /// pass over the file — otherwise the target verifies against a manifest
    /// the source never actually wrote.
    #[tokio::test]
    async fn writer_length_and_digest_match_the_file() -> Result {
        let dir = tmp()?;
        let mut writer = StagedWriter::create(dir.path(), RUN).await?;
        writer.write(b"first chunk ").await?;
        writer.write(b"second chunk").await?;
        assert_eq!(writer.staged_bytes(), 24);
        let (len, dgst) = writer.finish().await?;

        assert_eq!(len, 24);
        assert_eq!(staged_len(dir.path(), RUN).await?, Some(24));
        assert_eq!(digest(dir.path(), RUN).await?, dgst);

        let mut expected = Sha256::new();
        expected.update(b"first chunk second chunk");
        assert_eq!(dgst, hex::encode(expected.finalize()));
        Ok(())
    }

    /// A transfer that stops partway must resume from the staged length, and
    /// the result must be byte-identical to an uninterrupted one.
    #[tokio::test]
    async fn append_resumes_from_the_staged_length() -> Result {
        let dir = tmp()?;
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

        let mut offset = append(dir.path(), RUN, 0, &payload[..1000]).await?;
        assert_eq!(offset, 1000);

        offset = staged_len(dir.path(), RUN).await?.unwrap_or(0);
        assert_eq!(offset, 1000);

        offset = append(dir.path(), RUN, offset, &payload[1000..3000]).await?;
        assert_eq!(offset, 3000);
        offset = append(dir.path(), RUN, offset, &payload[3000..]).await?;
        assert_eq!(offset, payload.len() as u64);

        assert_eq!(
            read_range(dir.path(), RUN, 0, payload.len()).await?,
            payload
        );

        let mut expected = Sha256::new();
        expected.update(&payload);
        assert_eq!(
            digest(dir.path(), RUN).await?,
            hex::encode(expected.finalize())
        );
        Ok(())
    }

    /// A duplicate range — the shape a retry produces — must be refused, not
    /// appended twice. Silent duplication would corrupt the snapshot and only
    /// surface as a digest mismatch after the whole transfer.
    #[tokio::test]
    async fn append_refuses_a_duplicate_or_gapped_offset() -> Result {
        let dir = tmp()?;
        append(dir.path(), RUN, 0, b"0123456789").await?;

        let duplicate = append(dir.path(), RUN, 0, b"0123456789").await;
        assert!(
            duplicate.is_err(),
            "re-appending at offset 0 must be refused"
        );

        let gapped = append(dir.path(), RUN, 20, b"xx").await;
        assert!(gapped.is_err(), "appending past the end must be refused");

        assert_eq!(staged_len(dir.path(), RUN).await?, Some(10));
        Ok(())
    }

    /// A range that runs off the end returns a short read rather than an error:
    /// that is how the target detects the final piece.
    #[tokio::test]
    async fn read_range_is_short_at_the_end_and_errors_past_it() -> Result {
        let dir = tmp()?;
        append(dir.path(), RUN, 0, b"0123456789").await?;

        assert_eq!(read_range(dir.path(), RUN, 0, 4).await?, b"0123");
        assert_eq!(read_range(dir.path(), RUN, 8, 1024).await?, b"89");
        assert_eq!(read_range(dir.path(), RUN, 10, 512).await?, b"");

        assert!(
            read_range(dir.path(), RUN, 11, 1).await.is_err(),
            "an offset past the staged length must be an error, not an empty read"
        );
        Ok(())
    }

    /// Offsets are `u64` end to end. The old chunk protocol carried them as
    /// `u32`, which silently truncated past 4 GiB; this asserts the arithmetic
    /// here does not.
    #[tokio::test]
    async fn offsets_past_u32_max_are_representable() -> Result {
        let dir = tmp()?;
        let beyond = u64::from(u32::MAX) + 1;

        let Err(err) = append(dir.path(), RUN, beyond, b"x").await else {
            anyhow::bail!("appending at a gapped 4 GiB offset must fail");
        };
        let msg = err.to_string();
        assert!(
            msg.contains(&beyond.to_string()),
            "the error must report the full u64 offset, got: {msg}"
        );
        Ok(())
    }

    /// Two runs must not share a file, or one replication would overwrite
    /// another's snapshot.
    #[tokio::test]
    async fn distinct_runs_stage_into_distinct_files() -> Result {
        let dir = tmp()?;
        append(dir.path(), "run-a", 0, b"aaaa").await?;
        append(dir.path(), "run-b", 0, b"bb").await?;

        assert_eq!(staged_len(dir.path(), "run-a").await?, Some(4));
        assert_eq!(staged_len(dir.path(), "run-b").await?, Some(2));
        assert_ne!(
            staged_file(dir.path(), "run-a"),
            staged_file(dir.path(), "run-b")
        );
        Ok(())
    }

    /// An instance name carrying path separators must not escape the staging
    /// directory.
    #[tokio::test]
    async fn a_traversing_instance_name_stays_inside_the_staging_dir() -> Result {
        let dir = tmp()?;
        let hostile = "../../../../etc/passwd";
        append(dir.path(), hostile, 0, b"x").await?;

        let path = staged_file(dir.path(), hostile);
        assert_eq!(path.parent(), Some(staging_dir(dir.path()).as_path()));
        assert!(staged_len(dir.path(), hostile).await?.is_some());
        Ok(())
    }

    /// Discard must be idempotent — a cleanup path runs on both the success
    /// and failure routes.
    #[tokio::test]
    async fn discard_removes_the_snapshot_and_is_idempotent() -> Result {
        let dir = tmp()?;
        append(dir.path(), RUN, 0, b"payload").await?;
        assert!(staged_len(dir.path(), RUN).await?.is_some());

        discard(dir.path(), RUN).await?;
        assert_eq!(staged_len(dir.path(), RUN).await?, None);
        discard(dir.path(), RUN).await?;
        Ok(())
    }
}
