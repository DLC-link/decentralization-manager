//! Wallet-relayed ACS transfer: the pipe with a wallet in the middle.
//!
//! The add-party path runs [`super::pipe`] between two DecMan nodes over Noise:
//! the target pulls a block, feeds it straight into its open `ImportPartyAcs`
//! stream, and asks for the next. Nothing lands on disk and the size of the
//! party stops mattering.
//!
//! The tenant path wants the same property but has no node-to-node channel — a
//! partner's host is generally not in this node's mesh, so the wallet is the
//! transport. That inverts the direction on the importing side: the joiner
//! cannot pull, because the thing holding the bytes is an HTTP client it has
//! never heard of.
//!
//! So the pull is turned into a wait. The joiner runs one `import_party_acs`
//! for the whole transfer, and its `next_block` callback blocks on a channel
//! that the wallet's POSTs feed. The wallet is then a dumb relay: read block N
//! from the source, hand block N to the joiner, repeat. Neither node
//! accumulates, which is the whole point.
//!
//! Two things follow from holding Canton streams open across HTTP requests, and
//! both are handled here rather than left to the handlers:
//!
//! * **The joiner is disconnected from the synchronizer for the whole relay.**
//!   `ImportPartyAcs` requires it, and now that window is paced by a third
//!   party that can stall or vanish. So a session that goes quiet is reaped.
//!   Reaping **drops the sender** rather than aborting the task: the callback
//!   then fails, `import_party_acs` unwinds through its own reconnect-and-
//!   verify bracket, and the participant comes back. Aborting would strand it
//!   disconnected, which is the one outcome worse than a failed import.
//! * **There is no resume.** A break means a fresh export from block 1, exactly
//!   as the add-party path accepts. Asking for block 1 again is therefore the
//!   signal to throw the old session away and start over, not an error.

use std::{collections::HashMap, sync::Arc, time::Duration};

use sqlx::SqlitePool;
use tokio::{
    sync::{Mutex, mpsc},
    time::Instant,
};

use crate::{
    config::NodeConfig,
    error::Result,
    workflow::party_replication::{
        ReplicationTarget, import_party_acs,
        pipe::{ExportSession, PipeBlock},
    },
};

/// How long a relay session may sit untouched before it is reaped.
///
/// Generous, because the wallet is relaying real bytes between two nodes and a
/// slow link is not a stalled one. Bounded, because an export holds a Canton
/// stream and an import holds the participant off the synchronizer.
pub const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// How often the reaper looks for sessions past that timeout.
const REAP_INTERVAL: Duration = Duration::from_secs(30);

/// Blocks the joiner's import task has yet to consume.
///
/// Capacity one, so a wallet cannot run ahead of Canton: the POST carrying
/// block N+1 waits until the import has taken block N. That is the whole
/// backpressure mechanism, and it is why neither side needs a buffer.
const BLOCK_CHANNEL_CAPACITY: usize = 1;

/// One export held open on a source host between block requests.
struct ExportEntry {
    session: ExportSession,
    last_touched: Instant,
}

/// One import held open on a joining host between block requests.
struct ImportEntry {
    /// Feeds the import task's `next_block`. Dropping it is how a session is
    /// cancelled: the callback sees the channel close and fails, and the import
    /// unwinds through its reconnect bracket.
    blocks: mpsc::Sender<PipeBlock>,
    /// Resolves when `import_party_acs` returns, so the POST carrying the final
    /// block can report what happened.
    task: tokio::task::JoinHandle<Result<()>>,
    /// The next sequence number this session will accept.
    expected_seq: u64,
    last_touched: Instant,
}

/// Every wallet-relayed transfer this node is part of, keyed by the
/// replication's instance name.
#[derive(Default)]
pub struct RelaySessions {
    exports: Mutex<HashMap<String, Arc<Mutex<ExportEntry>>>>,
    imports: Mutex<HashMap<String, ImportEntry>>,
}

/// What a source did with a block request.
pub enum ServedBlock {
    /// The block, ready to hand to the wallet.
    Block(PipeBlock),
    /// `seq` is neither a replay of the last block nor the next one, and the
    /// Canton stream cannot rewind. The wallet must restart from block 1.
    OutOfSync { expected: u64, asked: u64 },
}

/// What a joiner did with a delivered block.
pub enum AcceptedBlock {
    /// Taken; the wallet should fetch the next one.
    Accepted { seq: u64 },
    /// The final block was consumed and the import finished.
    Complete,
    /// No session, or `seq` is not the one expected. The wallet must restart
    /// from block 1.
    OutOfSync { expected: Option<u64>, asked: u64 },
}

impl RelaySessions {
    pub fn new() -> Self {
        Self::default()
    }

    // ----------------------------------------------------------------
    // Source side
    // ----------------------------------------------------------------

    /// Serve block `seq` of this replication's export, opening the export on
    /// the first block.
    ///
    /// Block 1 always starts a fresh export. A wallet that asks for it after a
    /// session has served past it is retrying the transfer from the top, and
    /// since the stream cannot rewind the only honest answer is a new one.
    ///
    /// # Errors
    /// Propagates a failure to open the export or to read from Canton.
    pub async fn serve_block(
        &self,
        config: &NodeConfig,
        db: &SqlitePool,
        target: &ReplicationTarget,
        seq: u64,
        block_size: usize,
        open: impl AsyncFnOnce() -> Result<ExportSession>,
    ) -> Result<ServedBlock> {
        let entry = {
            let mut exports = self.exports.lock().await;
            match exports.get(&target.instance_name) {
                // Restarting: throw the old stream away rather than trying to
                // rewind it.
                Some(_) if seq == 1 => {
                    exports.remove(&target.instance_name);
                    None
                }
                Some(entry) => Some(entry.clone()),
                None => None,
            }
        };

        let entry = match entry {
            Some(entry) => entry,
            None => {
                if seq != 1 {
                    return Ok(ServedBlock::OutOfSync {
                        expected: 1,
                        asked: seq,
                    });
                }
                let _ = (config, db);
                let entry = Arc::new(Mutex::new(ExportEntry {
                    session: open().await?,
                    last_touched: Instant::now(),
                }));
                self.exports
                    .lock()
                    .await
                    .insert(target.instance_name.clone(), entry.clone());
                entry
            }
        };

        let mut guard = entry.lock().await;
        let served = guard.session.served_blocks();
        if seq != served && seq != served + 1 {
            return Ok(ServedBlock::OutOfSync {
                expected: served + 1,
                asked: seq,
            });
        }
        let block = guard.session.block(seq, block_size).await?;
        guard.last_touched = Instant::now();

        // The export is spent once the end is served; keeping it would only
        // hold a Canton stream open for a transfer that is over.
        if matches!(block, PipeBlock::End { .. }) {
            drop(guard);
            self.exports.lock().await.remove(&target.instance_name);
        }
        Ok(ServedBlock::Block(block))
    }

    // ----------------------------------------------------------------
    // Joiner side
    // ----------------------------------------------------------------

    /// Hand `block` to this replication's import, starting the import on the
    /// first block.
    ///
    /// Returns once the import has taken the block, which is what paces the
    /// wallet. On the final block it waits for the import to finish, so the
    /// caller can report the outcome rather than guessing it.
    ///
    /// # Errors
    /// Propagates an import failure, which by then has already put the
    /// participant back on the synchronizer.
    pub async fn accept_block(
        &self,
        config: &NodeConfig,
        db: &SqlitePool,
        target: &ReplicationTarget,
        required_package_ids: &[String],
        block: PipeBlock,
    ) -> Result<AcceptedBlock> {
        let seq = match &block {
            PipeBlock::Data { seq, .. } | PipeBlock::End { seq, .. } => *seq,
        };
        let is_end = matches!(block, PipeBlock::End { .. });

        let sender = {
            let mut imports = self.imports.lock().await;
            if seq == 1 {
                // A restart. The previous import must be fully retired before
                // the next one starts, not merely told to stop.
                //
                // Dropping the sender closes its channel, which fails the
                // callback and unwinds that import through its own
                // reconnect-and-verify bracket. That reconnect is the problem:
                // if the new import has already disconnected the participant
                // and begun feeding Canton by the time it lands, the old task
                // reconnects underneath it and the new import loses its stream
                // with bytes already imported — the orphan-ACS state the
                // quarantine exists to catch. An ordinary wallet retry after a
                // transient error would be enough to cause it.
                //
                // So the teardown is awaited, under the lock, before anything
                // new is spawned. Holding it cannot deadlock: a spawned import
                // never touches this registry.
                if let Some(old) = imports.remove(&target.instance_name) {
                    // Its failure is expected — it is being cancelled — so it is
                    // logged rather than returned over this fresh transfer.
                    if let Err(e) = retire(old).await {
                        tracing::info!("the previous ACS import ended: {e}");
                    }
                }
                let (tx, rx) = mpsc::channel(BLOCK_CHANNEL_CAPACITY);
                let task = spawn_import(
                    config.clone(),
                    db.clone(),
                    target.clone(),
                    required_package_ids.to_vec(),
                    rx,
                );
                imports.insert(
                    target.instance_name.clone(),
                    ImportEntry {
                        blocks: tx.clone(),
                        task,
                        expected_seq: 2,
                        last_touched: Instant::now(),
                    },
                );
                tx
            } else {
                let Some(entry) = imports.get_mut(&target.instance_name) else {
                    return Ok(AcceptedBlock::OutOfSync {
                        expected: None,
                        asked: seq,
                    });
                };
                if seq != entry.expected_seq {
                    let expected = entry.expected_seq;
                    return Ok(AcceptedBlock::OutOfSync {
                        expected: Some(expected),
                        asked: seq,
                    });
                }
                entry.expected_seq = seq + 1;
                entry.last_touched = Instant::now();
                entry.blocks.clone()
            }
        };

        // Awaits capacity, so this returns only once the import has taken the
        // previous block. A closed channel means the import already failed; the
        // error it failed with is surfaced by the join below.
        if sender.send(block).await.is_err() {
            return self.finish(target).await.map(|()| AcceptedBlock::Complete);
        }

        if is_end {
            self.finish(target).await?;
            return Ok(AcceptedBlock::Complete);
        }
        Ok(AcceptedBlock::Accepted { seq })
    }

    /// Wait for this replication's import to finish and take its result.
    async fn finish(&self, target: &ReplicationTarget) -> Result<()> {
        let Some(entry) = self.imports.lock().await.remove(&target.instance_name) else {
            anyhow::bail!(
                "no ACS import is open for {party} on this host; restart the transfer from \
                 block 1",
                party = target.party_id
            );
        };
        retire(entry).await
    }

    /// Drop this replication's sessions, if any.
    ///
    /// Used when a transfer is abandoned deliberately. The import is not
    /// aborted — see the module docs.
    pub async fn discard(&self, instance_name: &str) {
        self.exports.lock().await.remove(instance_name);
        let entry = self.imports.lock().await.remove(instance_name);
        if let Some(entry) = entry
            && let Err(e) = retire(entry).await
        {
            tracing::info!("the discarded ACS import ended: {e}");
        }
    }

    /// Remove sessions untouched for longer than `idle_timeout`.
    ///
    /// Returns how many of each went. An import removed here is not aborted:
    /// dropping its sender fails the callback, and `import_party_acs` reconnects
    /// the participant on the way out.
    pub async fn reap(&self, idle_timeout: Duration) -> (usize, usize) {
        let now = Instant::now();

        let mut exports = self.exports.lock().await;
        let mut stale = Vec::new();
        for (name, entry) in exports.iter() {
            if let Ok(guard) = entry.try_lock()
                && now.duration_since(guard.last_touched) > idle_timeout
            {
                stale.push(name.clone());
            }
        }
        for name in &stale {
            exports.remove(name);
        }
        let exports_reaped = stale.len();
        drop(exports);

        // Awaited, not merely removed. A reaped import is still unwinding
        // towards its reconnect, and a wallet that retries the moment the entry
        // disappears would otherwise race that reconnect against a fresh
        // import's disconnect. The reaper can afford to wait; the transfer it
        // is reaping has been idle for minutes.
        let mut imports = self.imports.lock().await;
        let stale: Vec<String> = imports
            .iter()
            .filter(|(_, entry)| now.duration_since(entry.last_touched) > idle_timeout)
            .map(|(name, _)| name.clone())
            .collect();
        for name in &stale {
            tracing::warn!(
                "reaping an ACS relay import idle for over {secs}s ({name}) — the wallet \
                 stopped relaying, so the import is failed and the participant reconnected",
                secs = idle_timeout.as_secs()
            );
            if let Some(entry) = imports.remove(name)
                && let Err(e) = retire(entry).await
            {
                tracing::info!("the reaped ACS import ended: {e}");
            }
        }
        (exports_reaped, stale.len())
    }
}

/// Close an import's channel and wait for it to unwind, returning what it
/// finished with.
///
/// The single way an import is ever torn down. Returning only once the task has
/// finished is what makes a teardown safe: the import has been through its own
/// reconnect-and-verify bracket before anything else is allowed to disconnect
/// the participant again. Removing the entry without this await lets the old
/// task's reconnect land underneath a new import that has already begun feeding
/// Canton, which is the orphan-ACS state the quarantine exists to catch.
async fn retire(entry: ImportEntry) -> Result<()> {
    // Dropping the sender is what the callback sees as "the wallet stopped
    // relaying"; the await is what makes the ordering observable.
    drop(entry.blocks);
    match entry.task.await {
        Ok(result) => result,
        Err(e) => anyhow::bail!("the ACS import task did not complete: {e}"),
    }
}

/// Run the whole import, pulling each block from `rx`.
fn spawn_import(
    config: NodeConfig,
    db: SqlitePool,
    target: ReplicationTarget,
    required_package_ids: Vec<String>,
    rx: mpsc::Receiver<PipeBlock>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let rx = Arc::new(Mutex::new(rx));
        import_party_acs(&config, &db, &target, &required_package_ids, move |seq| {
            let rx = rx.clone();
            async move {
                let Some(block) = rx.lock().await.recv().await else {
                    anyhow::bail!(
                        "the wallet stopped relaying before block {seq}; the transfer must \
                         restart from block 1"
                    );
                };
                let got = match &block {
                    PipeBlock::Data { seq, .. } | PipeBlock::End { seq, .. } => *seq,
                };
                anyhow::ensure!(
                    got == seq,
                    "the wallet relayed block {got} where block {seq} was expected"
                );
                Ok(block)
            }
        })
        .await
    })
}

/// Reap idle relay sessions forever.
///
/// Spawned once at startup. Without it a wallet that vanishes mid-transfer
/// leaves the joiner off the synchronizer indefinitely.
pub async fn reap_forever(sessions: Arc<RelaySessions>) {
    loop {
        tokio::time::sleep(REAP_INTERVAL).await;
        let (exports, imports) = sessions.reap(SESSION_IDLE_TIMEOUT).await;
        if exports + imports > 0 {
            tracing::info!("reaped {exports} idle ACS export(s) and {imports} import(s)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        canton_id::CantonId,
        workflow::party_replication::{ArtifactStore, ReplicationArtifacts},
    };

    fn target(instance: &str) -> ReplicationTarget {
        let id = |p: &str| match CantonId::parse(&format!("{p}::1220{}", "aa".repeat(32))) {
            Ok(id) => id,
            Err(e) => panic!("test id must parse: {e}"),
        };
        ReplicationTarget {
            party_id: id("alice"),
            target_participant_id: id("participant-3"),
            instance_name: instance.to_string(),
            artifacts: ReplicationArtifacts {
                export_offset: "export_offset",
                pre_activation_offset: "pre_activation_offset",
                import_inflight: "import_inflight",
            },
            store: ArtifactStore::Tenant,
        }
    }

    async fn pool() -> sqlx::SqlitePool {
        match sqlx::SqlitePool::connect("sqlite::memory:").await {
            Ok(p) => p,
            Err(e) => panic!("in-memory sqlite must open: {e}"),
        }
    }

    /// A wallet that starts mid-stream has nothing to resume onto: the export is
    /// opened by block 1 and cannot seek. Refusing here is what turns that into
    /// a 409 telling the wallet to restart, rather than an opened export that
    /// silently serves the wrong bytes.
    #[tokio::test]
    async fn serving_a_later_block_with_no_session_is_out_of_sync() {
        let sessions = RelaySessions::new();
        let db = pool().await;
        let result = sessions
            .serve_block(
                &NodeConfig::default(),
                &db,
                &target("run-1"),
                7,
                1024,
                async || panic!("no export may be opened for a block past the first"),
            )
            .await;
        match result {
            Ok(ServedBlock::OutOfSync { expected, asked }) => {
                assert_eq!((expected, asked), (1, 7));
            }
            Ok(ServedBlock::Block(_)) => panic!("block 7 cannot be served from nothing"),
            Err(e) => panic!("this is a protocol answer, not a failure: {e}"),
        }
    }

    /// The mirror on the joiner. Without a session there is no open import to
    /// feed, and starting one mid-stream would hand Canton a truncated ACS.
    #[tokio::test]
    async fn accepting_a_later_block_with_no_session_is_out_of_sync() {
        let sessions = RelaySessions::new();
        let db = pool().await;
        let block = PipeBlock::Data {
            seq: 4,
            bytes: b"some bytes".to_vec(),
        };
        let result = sessions
            .accept_block(&NodeConfig::default(), &db, &target("run-1"), &[], block)
            .await;
        match result {
            Ok(AcceptedBlock::OutOfSync { expected, asked }) => {
                assert_eq!((expected, asked), (None, 4));
            }
            Ok(_) => panic!("block 4 cannot be imported without an open session"),
            Err(e) => panic!("this is a protocol answer, not a failure: {e}"),
        }
    }

    /// A stand-in for `import_party_acs`: pulls blocks until the channel
    /// closes, then does what the real one's reconnect bracket does — some work
    /// on the way out. `finished` flips only after that work.
    fn fake_import(
        mut rx: mpsc::Receiver<PipeBlock>,
        taken: Arc<std::sync::atomic::AtomicUsize>,
        finished: Arc<std::sync::atomic::AtomicBool>,
    ) -> tokio::task::JoinHandle<Result<()>> {
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                taken.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            // Stands for reconnect-and-verify: the window during which the old
            // import is still touching the participant.
            tokio::time::sleep(Duration::from_millis(50)).await;
            finished.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }

    fn entry_for(
        tx: mpsc::Sender<PipeBlock>,
        task: tokio::task::JoinHandle<Result<()>>,
    ) -> ImportEntry {
        ImportEntry {
            blocks: tx,
            task,
            expected_seq: 2,
            last_touched: Instant::now(),
        }
    }

    /// The bug this guards: removing an import's entry only *starts* its
    /// unwind. If the next import is allowed to disconnect the participant
    /// while the old one is still heading for its reconnect, that reconnect
    /// lands underneath a stream with bytes already in Canton — the orphan-ACS
    /// state the quarantine exists to catch, reachable from an ordinary wallet
    /// retry.
    #[tokio::test]
    async fn retiring_an_import_waits_for_it_to_finish_unwinding() {
        let (tx, rx) = mpsc::channel(BLOCK_CHANNEL_CAPACITY);
        let taken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = fake_import(rx, taken.clone(), finished.clone());

        if tx
            .send(PipeBlock::Data {
                seq: 1,
                bytes: b"first".to_vec(),
            })
            .await
            .is_err()
        {
            panic!("the import must take the first block");
        }

        if let Err(e) = retire(entry_for(tx, task)).await {
            panic!("retiring a healthy import must not fail: {e}");
        }
        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "retire returned before the import finished unwinding, so a new import \
             could disconnect the participant underneath it"
        );
        assert_eq!(taken.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// The channel is the whole backpressure mechanism: at capacity one, the
    /// POST carrying block N+1 cannot return until Canton has taken block N.
    /// Without it the joiner would have to buffer, which is the property this
    /// design exists to avoid.
    #[tokio::test]
    async fn a_second_block_waits_until_the_import_has_taken_the_first() {
        let (tx, mut rx) = mpsc::channel::<PipeBlock>(BLOCK_CHANNEL_CAPACITY);
        let block = |seq| PipeBlock::Data {
            seq,
            bytes: vec![0u8; 4],
        };

        if tx.send(block(1)).await.is_err() {
            panic!("the first block fits in the channel");
        }
        // Nothing has consumed block 1, so block 2 has nowhere to go.
        assert!(
            tx.try_send(block(2)).is_err(),
            "a second block must not be accepted before the import takes the first"
        );

        let Some(_) = rx.recv().await else {
            panic!("block 1 must be there to take")
        };
        if tx.try_send(block(2)).is_err() {
            panic!("once the import has taken a block, the next one fits");
        }
    }

    /// Restarting at block 1 must retire the previous import before spawning
    /// the next, not merely forget it.
    #[tokio::test]
    async fn restarting_retires_the_previous_import_first() {
        let sessions = RelaySessions::new();
        let target = target("run-1");
        let (tx, rx) = mpsc::channel(BLOCK_CHANNEL_CAPACITY);
        let taken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = fake_import(rx, taken.clone(), finished.clone());
        sessions
            .imports
            .lock()
            .await
            .insert(target.instance_name.clone(), entry_for(tx, task));

        // `discard` is the teardown path a caller reaches for directly; it
        // shares `retire` with the restart branch.
        sessions.discard(&target.instance_name).await;

        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "the previous import must be fully unwound before the registry lets go of it"
        );
        assert!(sessions.imports.lock().await.is_empty());
    }

    /// Discarding a replication that has no sessions is the ordinary case on a
    /// transfer that never started, so it must not panic or fail.
    #[tokio::test]
    async fn discarding_an_unknown_replication_is_a_no_op() {
        let sessions = RelaySessions::new();
        sessions.discard("never-started").await;
        assert_eq!(sessions.reap(Duration::ZERO).await, (0, 0));
    }
}
