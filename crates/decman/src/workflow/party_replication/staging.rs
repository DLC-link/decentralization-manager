//! On-disk staging for a replicated ACS snapshot.
//!
//! The wallet-relayed path used to carry the whole snapshot base64-encoded in a
//! JSON body. That put a hard ceiling on it well below what the export allowed —
//! actix caps JSON at 100 MiB and base64 inflates by 4/3, so a snapshot over
//! ~75 MiB was refused with a 413 no matter what the export cap said — and it
//! meant a transfer that failed at 90% started again from zero.
//!
//! So the snapshot lands in a file and moves in bounded pieces. The exporter
//! stages once and serves byte ranges out of it; the importer appends pieces
//! until it has the whole thing. Neither side holds more than one piece in
//! memory, and either side can say how far it got, which is what makes a
//! resumable transfer possible at all.
//!
//! Deliberately not tus. The direction is inverted for the export half (our
//! receiver is the client), no embeddable tus server exists for Rust, and it
//! would mean speaking HTTP semantics we do not otherwise need. What tus is
//! actually worth here is its state model — an offset as the protocol primitive
//! — and that is a `u64` and a file, not a dependency.

use std::path::PathBuf;

use anyhow::Context;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::{config::NodeConfig, error::Result};

/// Where a replication's staged snapshots live.
pub(crate) fn staging_dir(config: &NodeConfig) -> PathBuf {
    config.data_dir().join("acs-staging")
}

/// The file a given replication stages into.
///
/// Named from the replication's instance name, which both sides derive from the
/// (party, target) pair, so a resumed transfer finds the same file without
/// anyone having to remember a handle.
pub(crate) fn staging_path(config: &NodeConfig, instance_name: &str) -> PathBuf {
    // The instance name embeds a party id, which contains `::` and could contain
    // path separators on a malformed input. Hashing keeps it a single flat file
    // name that cannot escape the directory.
    let mut hasher = Sha256::new();
    hasher.update(instance_name.as_bytes());
    let digest = hex::encode(hasher.finalize());
    staging_dir(config).join(format!("{digest}.acs"))
}

/// Where an export writes before it is complete.
///
/// A staged file is only ever seen at its final path, whole. Writing in place
/// would let a second caller read a growing file, see its current length as the
/// snapshot's `total_size`, and relay a truncated ACS the joiner would import
/// and go live on.
fn staging_temp_path(config: &NodeConfig, instance_name: &str) -> PathBuf {
    let mut path = staging_path(config, instance_name).into_os_string();
    path.push(".part");
    PathBuf::from(path)
}

/// How long a `.part` file is treated as an export still in flight.
///
/// Past this it is assumed to belong to a process that died, and the next
/// export takes it over. Without that a crash would wedge the replication
/// permanently.
const EXPORT_CLAIM_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// How long a completed staged snapshot is kept before [`sweep`] reclaims it.
///
/// The source has no signal that the wallet is finished with a snapshot — the
/// wallet may resume a transfer hours later — so the file is reclaimed on age
/// rather than on an event. A day is comfortably longer than any transfer and
/// short enough that an abandoned one is not a permanent copy of the party's
/// contracts.
pub const STAGING_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// The path an in-flight export streams into.
///
/// Only for a caller that already holds the claim from [`begin_export`]. Finish
/// with [`finish_export`], which is what makes the snapshot visible.
pub fn claim_path(config: &NodeConfig, instance_name: &str) -> PathBuf {
    staging_temp_path(config, instance_name)
}

/// Publish an export streamed into [`claim_path`], returning its size.
///
/// The rename is the whole point: until it happens the staged path holds
/// nothing, so a concurrent range request cannot read a growing file and take
/// its current length for the snapshot's size.
///
/// # Errors
/// Returns an error if the claim is missing or cannot be renamed.
pub async fn finish_export(config: &NodeConfig, instance_name: &str) -> Result<u64> {
    let temp = staging_temp_path(config, instance_name);
    let len = tokio::fs::metadata(&temp)
        .await
        .with_context(|| format!("inspecting the streamed ACS at {}", temp.display()))?
        .len();
    let path = staging_path(config, instance_name);
    tokio::fs::rename(&temp, &path).await.with_context(|| {
        format!(
            "moving the staged ACS from {} to {}",
            temp.display(),
            path.display()
        )
    })?;
    Ok(len)
}

/// Claim the right to export this replication, returning `false` if another
/// export already holds it.
///
/// The claim is the `.part` file itself, created exclusively. A caller that gets
/// `true` must finish with [`stage`] or [`abandon_export`].
///
/// # Errors
/// Returns an error if the staging directory or the claim cannot be written.
pub async fn begin_export(config: &NodeConfig, instance_name: &str) -> Result<bool> {
    let dir = staging_dir(config);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("creating the ACS staging directory {}", dir.display()))?;
    let path = staging_temp_path(config, instance_name);
    match tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .await
    {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // An export that died left its claim behind. Nothing else can tell
            // the difference, so age decides.
            let stale = match tokio::fs::metadata(&path).await {
                Ok(meta) => meta
                    .modified()
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .is_some_and(|age| age > EXPORT_CLAIM_TTL),
                Err(_) => false,
            };
            if !stale {
                return Ok(false);
            }
            tokio::fs::remove_file(&path).await.with_context(|| {
                format!("reclaiming the stale export claim at {}", path.display())
            })?;
            Ok(true)
        }
        Err(e) => {
            Err(anyhow::Error::new(e).context(format!("claiming the export at {}", path.display())))
        }
    }
}

/// Release a claim taken by [`begin_export`] without staging anything.
///
/// # Errors
/// Returns an error only if the claim exists and cannot be removed.
pub async fn abandon_export(config: &NodeConfig, instance_name: &str) -> Result<()> {
    let path = staging_temp_path(config, instance_name);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::Error::new(e)
            .context(format!("releasing the export claim at {}", path.display()))),
    }
}

/// Delete staged files and export claims older than `ttl`.
///
/// Nothing else reclaims a source's staged snapshot: the joiner discards its own
/// copy once the import lands, but the exporting host is never told the wallet
/// is done. Returns how many files were removed.
///
/// # Errors
/// Returns an error if the directory cannot be read. A file that cannot be
/// removed is logged and skipped, since one stuck file should not stop the rest.
pub async fn sweep(config: &NodeConfig, ttl: std::time::Duration) -> Result<usize> {
    let dir = staging_dir(config);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!(
                "reading the ACS staging directory {}",
                dir.display()
            )));
        }
    };
    let mut removed = 0usize;
    while let Some(entry) = entries
        .next_entry()
        .await
        .with_context(|| format!("listing {}", dir.display()))?
    {
        let path = entry.path();
        let old = match entry.metadata().await {
            Ok(meta) => meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age > ttl),
            Err(_) => false,
        };
        if !old {
            continue;
        }
        match tokio::fs::remove_file(&path).await {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("could not sweep {}: {e}", path.display()),
        }
    }
    Ok(removed)
}

/// Write `bytes` as this replication's staged snapshot, replacing any previous
/// one.
///
/// # Errors
/// Returns an error if the staging directory or file cannot be written.
pub async fn stage(config: &NodeConfig, instance_name: &str, bytes: &[u8]) -> Result<()> {
    let dir = staging_dir(config);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("creating the ACS staging directory {}", dir.display()))?;
    // Written to the claim path and renamed, so the snapshot only ever appears
    // at its final path complete. A reader that saw it growing would take the
    // bytes written so far for the whole snapshot.
    let temp = staging_temp_path(config, instance_name);
    tokio::fs::write(&temp, bytes)
        .await
        .with_context(|| format!("staging the ACS snapshot to {}", temp.display()))?;
    let path = staging_path(config, instance_name);
    tokio::fs::rename(&temp, &path).await.with_context(|| {
        format!(
            "moving the staged ACS from {} to {}",
            temp.display(),
            path.display()
        )
    })?;
    Ok(())
}

/// How many bytes of this replication's snapshot are staged, or `None` when
/// nothing is.
///
/// # Errors
/// Returns an error if the file exists but cannot be inspected.
pub async fn staged_len(config: &NodeConfig, instance_name: &str) -> Result<Option<u64>> {
    let path = staging_path(config, instance_name);
    match tokio::fs::metadata(&path).await {
        Ok(meta) => Ok(Some(meta.len())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e)
            .context(format!("inspecting the staged ACS at {}", path.display()))),
    }
}

/// Read at most `limit` bytes of the staged snapshot from `offset`.
///
/// A short read means the end of the snapshot, which is how the caller knows it
/// has everything without being told a count separately.
///
/// # Errors
/// Returns an error if nothing is staged, or the read fails.
pub async fn read_range(
    config: &NodeConfig,
    instance_name: &str,
    offset: u64,
    limit: usize,
) -> Result<Vec<u8>> {
    let path = staging_path(config, instance_name);
    let mut file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("opening the staged ACS at {}", path.display()))?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .with_context(|| format!("seeking to {offset} in {}", path.display()))?;
    let mut buf = vec![0u8; limit];
    let mut filled = 0usize;
    // One `read` can return short without being at the end, so read until the
    // buffer is full or the file really is exhausted.
    while filled < limit {
        let n = file
            .read(&mut buf[filled..])
            .await
            .with_context(|| format!("reading {} at offset {offset}", path.display()))?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(buf)
}

/// Append `chunk` at `offset`, returning the total bytes staged afterwards.
///
/// `offset` must be exactly what is already staged. A mismatch means the sender
/// and receiver disagree about progress, and appending anyway would produce a
/// snapshot with a hole or a duplicated span — corruption Canton would only
/// discover mid-import, after the participant has disconnected.
///
/// # Errors
/// Returns an error on an offset mismatch, or if the write fails.
pub async fn append(
    config: &NodeConfig,
    instance_name: &str,
    offset: u64,
    chunk: &[u8],
) -> Result<u64> {
    let dir = staging_dir(config);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("creating the ACS staging directory {}", dir.display()))?;
    let path = staging_path(config, instance_name);

    let current = staged_len(config, instance_name).await?.unwrap_or(0);
    if offset != current {
        anyhow::bail!(
            "chunk claims offset {offset} but {current} byte(s) are staged; resume from {current} \
             rather than writing a snapshot with a hole in it"
        );
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .with_context(|| format!("opening {} to append", path.display()))?;
    file.write_all(chunk)
        .await
        .with_context(|| format!("appending {} byte(s) to {}", chunk.len(), path.display()))?;
    file.flush()
        .await
        .with_context(|| format!("flushing {}", path.display()))?;

    Ok(current + chunk.len() as u64)
}

/// Read the whole staged snapshot, for handing to Canton's import.
///
/// # Errors
/// Returns an error if nothing is staged or the read fails.
pub async fn read_all(config: &NodeConfig, instance_name: &str) -> Result<Vec<u8>> {
    let path = staging_path(config, instance_name);
    tokio::fs::read(&path)
        .await
        .with_context(|| format!("reading the staged ACS at {}", path.display()))
}

/// Delete this replication's staged snapshot.
///
/// Called once the import has landed. Missing is success: the point is that
/// nothing is left behind, and a caller retrying a cleanup should not fail.
///
/// # Errors
/// Returns an error only if the file exists and cannot be removed.
pub async fn discard(config: &NodeConfig, instance_name: &str) -> Result<()> {
    abandon_export(config, instance_name).await?;
    let path = staging_path(config, instance_name);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::Error::new(e)
            .context(format!("discarding the staged ACS at {}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config rooted at a fresh temp dir, so each test stages in isolation.
    fn config(dir: &std::path::Path) -> NodeConfig {
        NodeConfig::default().with_root_dir(dir)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "decman-staging-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[tokio::test]
    async fn nothing_is_staged_before_anything_is_written() {
        let dir = temp_dir("empty");
        let config = config(&dir);
        let Ok(len) = staged_len(&config, "run").await else {
            panic!("inspecting an unstaged replication must succeed");
        };
        assert!(len.is_none());
    }

    /// The ranges must stitch back into exactly what was staged, or the joiner
    /// imports a snapshot that is not the one the source exported.
    #[tokio::test]
    async fn ranges_reassemble_into_the_original() -> anyhow::Result<()> {
        let dir = temp_dir("ranges");
        let config = config(&dir);
        let snapshot: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        stage(&config, "run", &snapshot).await?;

        let mut rebuilt = Vec::new();
        let mut offset = 0u64;
        loop {
            let chunk = read_range(&config, "run", offset, 512).await?;
            if chunk.is_empty() {
                break;
            }
            offset += chunk.len() as u64;
            rebuilt.extend_from_slice(&chunk);
        }
        assert_eq!(rebuilt, snapshot);
        Ok(())
    }

    /// A short read is how the caller learns it has everything, so it must only
    /// happen at the end.
    #[tokio::test]
    async fn a_full_range_is_returned_when_more_remains() -> anyhow::Result<()> {
        let dir = temp_dir("short");
        let config = config(&dir);
        stage(&config, "run", &vec![7u8; 1000]).await?;

        assert_eq!(read_range(&config, "run", 0, 400).await?.len(), 400);
        // 600 left, asked for 400: still full.
        assert_eq!(read_range(&config, "run", 400, 400).await?.len(), 400);
        // 200 left, asked for 400: short, meaning the end.
        assert_eq!(read_range(&config, "run", 800, 400).await?.len(), 200);
        Ok(())
    }

    #[tokio::test]
    async fn appends_accumulate_and_report_progress() -> anyhow::Result<()> {
        let dir = temp_dir("append");
        let config = config(&dir);

        assert_eq!(append(&config, "run", 0, b"abc").await?, 3);
        assert_eq!(append(&config, "run", 3, b"defg").await?, 7);
        assert_eq!(read_all(&config, "run").await?, b"abcdefg");
        Ok(())
    }

    /// The invariant that matters most: a wrong offset is refused rather than
    /// written. Writing it would leave a hole or a duplicated span, and Canton
    /// would only discover that mid-import, with the participant already
    /// disconnected.
    #[tokio::test]
    async fn refuses_an_offset_that_does_not_match_what_is_staged() -> anyhow::Result<()> {
        let dir = temp_dir("mismatch");
        let config = config(&dir);
        append(&config, "run", 0, b"abc").await?;

        // A gap.
        let Err(e) = append(&config, "run", 10, b"xyz").await else {
            panic!("an offset past what is staged must be refused");
        };
        assert!(e.to_string().contains("resume from 3"), "{e}");

        // A replay.
        let Err(e) = append(&config, "run", 0, b"abc").await else {
            panic!("re-sending an already-staged range must be refused");
        };
        assert!(e.to_string().contains("resume from 3"), "{e}");

        // Unchanged by either refusal.
        assert_eq!(read_all(&config, "run").await?, b"abc");
        Ok(())
    }

    /// Two replications must not share a file, or one party's contracts land in
    /// another party's import.
    #[tokio::test]
    async fn separate_replications_stage_separately() -> anyhow::Result<()> {
        let dir = temp_dir("distinct");
        let config = config(&dir);
        append(&config, "tenant-add-hosts:alice:p3", 0, b"alice").await?;
        append(&config, "tenant-add-hosts:bob:p3", 0, b"bob").await?;

        assert_eq!(
            read_all(&config, "tenant-add-hosts:alice:p3").await?,
            b"alice"
        );
        assert_eq!(read_all(&config, "tenant-add-hosts:bob:p3").await?, b"bob");
        Ok(())
    }

    /// A party id contains `::` and could contain a separator on malformed
    /// input; the staged name must stay one flat file inside the directory.
    #[tokio::test]
    async fn a_hostile_instance_name_cannot_escape_the_staging_directory() -> anyhow::Result<()> {
        let dir = temp_dir("escape");
        let config = config(&dir);
        append(&config, "../../etc/passwd", 0, b"nope").await?;

        let staged = staging_path(&config, "../../etc/passwd");
        assert_eq!(staged.parent(), Some(staging_dir(&config).as_path()));
        Ok(())
    }

    /// The ceiling that bounds the Noise path is a `u32` `PayloadLength`, which
    /// truncates silently past 4 GiB. This path is `u64` end to end, and this
    /// proves it: offsets and range arithmetic past `u32::MAX` must work rather
    /// than wrap.
    ///
    /// Sparse: `set_len` allocates no blocks, so a 5 GiB file costs nothing on
    /// any filesystem that supports holes. The bytes are irrelevant — the
    /// arithmetic is what is under test, and materialising 5 GiB to check a
    /// `u64` seek would be a waste of a CI runner.
    #[tokio::test]
    async fn offsets_past_u32_max_are_not_truncated() -> anyhow::Result<()> {
        const BEYOND_U32: u64 = 5 * 1024 * 1024 * 1024;
        let dir = temp_dir("wide");
        let config = config(&dir);

        tokio::fs::create_dir_all(staging_dir(&config)).await?;
        let path = staging_path(&config, "run");
        let file = tokio::fs::File::create(&path).await?;
        file.set_len(BEYOND_U32).await?;
        drop(file);

        // The reported length must be the real one, not a wrapped u32.
        assert_eq!(staged_len(&config, "run").await?, Some(BEYOND_U32));
        assert!(BEYOND_U32 > u64::from(u32::MAX));

        // A read seeked past 4 GiB must land where it was asked to, not at
        // `offset % 2^32`.
        let far = u64::from(u32::MAX) + 1000;
        assert_eq!(read_range(&config, "run", far, 256).await?.len(), 256);

        // Appending continues from the real end.
        assert_eq!(
            append(&config, "run", BEYOND_U32, b"tail").await?,
            BEYOND_U32 + 4
        );
        // And an offset that wrapped to something small is still refused.
        let Err(e) = append(&config, "run", 1000, b"nope").await else {
            panic!("a wrapped offset must be refused");
        };
        assert!(e.to_string().contains("resume from"), "{e}");

        discard(&config, "run").await?;
        Ok(())
    }

    #[tokio::test]
    async fn discarding_is_idempotent() -> anyhow::Result<()> {
        let dir = temp_dir("discard");
        let config = config(&dir);
        append(&config, "run", 0, b"abc").await?;

        discard(&config, "run").await?;
        assert!(staged_len(&config, "run").await?.is_none());
        // Missing is success: a retried cleanup must not fail.
        discard(&config, "run").await?;
        Ok(())
    }

    /// A second caller during a long export must not see the partial file. The
    /// claim is what stops it, and the export writes through a path the reader
    /// never looks at.
    #[tokio::test]
    async fn a_second_export_is_refused_while_one_is_running() -> anyhow::Result<()> {
        let dir = temp_dir("claim");
        let config = config(&dir);

        assert!(begin_export(&config, "run-1").await?);
        assert!(
            !begin_export(&config, "run-1").await?,
            "a second export must be refused rather than overwrite the first"
        );
        // Nothing is readable at the staged path while the export runs, so a
        // reader cannot mistake a partial file for the whole snapshot.
        assert_eq!(staged_len(&config, "run-1").await?, None);

        stage(&config, "run-1", b"the snapshot").await?;
        assert_eq!(staged_len(&config, "run-1").await?, Some(12));
        // The claim is gone with the rename, so the next export can start.
        assert!(begin_export(&config, "run-1").await?);
        Ok(())
    }

    /// Two replications export independently: one claim must not block the
    /// other.
    #[tokio::test]
    async fn claims_are_per_replication() -> anyhow::Result<()> {
        let dir = temp_dir("claim-scope");
        let config = config(&dir);

        assert!(begin_export(&config, "run-1").await?);
        assert!(begin_export(&config, "run-2").await?);
        Ok(())
    }

    /// An export that dies leaves its claim behind. Releasing it must let the
    /// next one through, or the replication is wedged.
    #[tokio::test]
    async fn abandoning_a_claim_frees_it() -> anyhow::Result<()> {
        let dir = temp_dir("abandon");
        let config = config(&dir);

        assert!(begin_export(&config, "run-1").await?);
        abandon_export(&config, "run-1").await?;
        assert!(begin_export(&config, "run-1").await?);
        Ok(())
    }

    /// The source is never told the wallet is finished, so its staged snapshot
    /// is reclaimed on age. A zero TTL makes every file old.
    #[tokio::test]
    async fn the_sweep_reclaims_staged_snapshots() -> anyhow::Result<()> {
        let dir = temp_dir("sweep");
        let config = config(&dir);

        stage(&config, "run-1", b"contracts").await?;
        stage(&config, "run-2", b"more contracts").await?;
        assert!(begin_export(&config, "run-3").await?);

        assert_eq!(sweep(&config, std::time::Duration::ZERO).await?, 3);
        assert_eq!(staged_len(&config, "run-1").await?, None);
        assert_eq!(staged_len(&config, "run-2").await?, None);
        Ok(())
    }

    /// A snapshot still inside its TTL is left alone: sweeping a transfer that
    /// is still running would be worse than keeping the file.
    #[tokio::test]
    async fn the_sweep_leaves_fresh_snapshots_alone() -> anyhow::Result<()> {
        let dir = temp_dir("sweep-fresh");
        let config = config(&dir);

        stage(&config, "run-1", b"contracts").await?;
        assert_eq!(sweep(&config, STAGING_TTL).await?, 0);
        assert_eq!(staged_len(&config, "run-1").await?, Some(9));
        Ok(())
    }

    /// Sweeping before anything has been staged is a no-op, not an error: it
    /// runs on every node at startup, including ones that never replicate.
    #[tokio::test]
    async fn the_sweep_tolerates_a_missing_directory() -> anyhow::Result<()> {
        let dir = temp_dir("sweep-empty");
        assert_eq!(sweep(&config(&dir), std::time::Duration::ZERO).await?, 0);
        Ok(())
    }
}
