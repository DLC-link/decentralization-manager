//! Generic workflow state machine.
//!
//! `WorkflowState<S>` holds the live state for a single workflow run on this
//! node — the current step, the set of expected peers, who's connected, and
//! the buffered command/peer data — and writes through to the persisted
//! `workflow_runs` row so the run survives a restart.

use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    sync::Arc,
};

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::{
    canton_id::CantonId,
    db::schema::{Commitable, SchemaWrite},
    noise::MessageType,
    server::{WorkflowKind, WorkflowProgress},
};

/// Trait for workflow steps. Implementations are small `Copy` enums per
/// workflow kind (Onboarding, Kick, Contracts, Dars).
pub trait WorkflowStep:
    Copy + std::fmt::Debug + PartialEq + Eq + std::hash::Hash + Send + Sync
{
    fn to_command(&self) -> Option<MessageType>;
    fn next(&self) -> Option<Self>;
    fn requires_peers(&self) -> bool;
    fn is_waiting_for_peers(&self) -> bool;

    /// Stable index of this variant (0..step_total). Used for the persisted
    /// `step_index` column on `workflow_runs` — the frontend renders progress
    /// as `step_index + 1 / step_total`.
    fn step_index(&self) -> i64;

    /// Total number of variants. Each impl is a small `const` in the impl body.
    fn step_total() -> i64;

    /// Stable string name for this variant. Matches the Debug-formatted name
    /// (e.g. `"SignDns"`). Used as the persisted `current_step` column.
    fn step_name(&self) -> &'static str;

    /// Reverse of `step_name`, used to re-hydrate `WorkflowState` from a
    /// persisted row at resume time.
    fn try_from_step_name(name: &str) -> Option<Self>;

    /// The workflow kind this step enum belongs to. Used to reject routed
    /// messages (e.g. invitation declines) that target a different kind.
    fn kind() -> WorkflowKind;
}

/// Generic workflow state tracker. Reads/writes the matching `workflow_runs`
/// row through `db` so a node restart can pick the run back up.
pub struct WorkflowState<S> {
    db: SqlitePool,
    instance_name: String,
    /// Current workflow step
    current_step: RwLock<S>,
    /// Expected peer IDs
    expected_peers: HashSet<CantonId>,
    /// Peer quorum for both gates; `None` requires all expected peers.
    peer_threshold: Option<usize>,
    /// Peers that have connected (transient — not persisted, recoverable
    /// via Noise reconnect after a restart)
    connected_peers: RwLock<HashSet<CantonId>>,
    /// Peers that have completed the current step
    completed_peers: RwLock<HashSet<CantonId>>,
    /// Data received from peers (e.g., keys, signatures)
    peer_data: RwLock<HashMap<CantonId, Vec<u8>>>,
    /// Payload data to send with the next command (e.g., proposals for signing)
    command_payload: RwLock<Vec<u8>>,
    _p: PhantomData<()>,
}

impl<S: WorkflowStep + 'static> WorkflowState<S> {
    /// Construct a fresh workflow state. Caller is expected to have already
    /// inserted a `workflow_runs` row for `instance_name` — this struct only
    /// updates the row, it doesn't create it.
    pub fn new(
        db: SqlitePool,
        instance_name: String,
        initial_step: S,
        expected_peers: Vec<CantonId>,
        peer_threshold: Option<usize>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            instance_name,
            current_step: RwLock::new(initial_step),
            expected_peers: expected_peers.into_iter().collect(),
            peer_threshold,
            connected_peers: RwLock::new(HashSet::new()),
            completed_peers: RwLock::new(HashSet::new()),
            peer_data: RwLock::new(HashMap::new()),
            command_payload: RwLock::new(Vec::new()),
            _p: PhantomData,
        })
    }

    /// Re-hydrate from a persisted `workflow_runs` row. The previously-completed
    /// peers (for the current step) are restored so the run picks back up
    /// without losing partial progress.
    pub fn from_persisted(
        db: SqlitePool,
        instance_name: String,
        current_step: S,
        expected_peers: Vec<CantonId>,
        completed_peers: Vec<CantonId>,
        peer_threshold: Option<usize>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            instance_name,
            current_step: RwLock::new(current_step),
            expected_peers: expected_peers.into_iter().collect(),
            peer_threshold,
            connected_peers: RwLock::new(HashSet::new()),
            completed_peers: RwLock::new(completed_peers.into_iter().collect()),
            peer_data: RwLock::new(HashMap::new()),
            command_payload: RwLock::new(Vec::new()),
            _p: PhantomData,
        })
    }

    pub fn instance_name(&self) -> &str {
        &self.instance_name
    }

    /// Set payload data to be sent with the next command
    pub async fn set_command_payload(&self, payload: Vec<u8>) {
        let mut cmd_payload = self.command_payload.write().await;
        *cmd_payload = payload;
    }

    /// Get payload data to send with command (clones the data)
    pub async fn get_command_payload(&self) -> Vec<u8> {
        self.command_payload.read().await.clone()
    }

    /// Clear the command payload
    pub async fn clear_command_payload(&self) {
        let mut cmd_payload = self.command_payload.write().await;
        cmd_payload.clear();
    }

    pub async fn current_step(&self) -> S {
        *self.current_step.read().await
    }

    /// `None`/no-peer → all; else clamped into `[1, total]` (the `>= 1` matters:
    /// gates fire on peer events, so a `0` requirement would never trip).
    fn peers_quorum(&self) -> usize {
        let total = self.expected_peers.len();
        match self.peer_threshold {
            Some(k) if total > 0 => k.clamp(1, total),
            _ => total,
        }
    }

    /// Re-evaluate the current step's peer gate and advance if it is satisfied.
    /// Called on every peer poll and completion (not just the triggering event),
    /// so a gate that became satisfiable without a fresh event — e.g. a quorum
    /// restored from the persisted row after a restart — still advances the next
    /// time a peer polls. Advancing goes through [`Self::advance_step_if`], so
    /// concurrent callers can't double-advance.
    async fn maybe_advance(&self) {
        let cur = *self.current_step.read().await;
        let quorum = self.peers_quorum();
        let ready = if cur.is_waiting_for_peers() {
            self.connected_peers.read().await.len() >= quorum
        } else if cur.requires_peers() {
            // Advance as soon as the quorum has signed. M signatures satisfy an
            // M-of-N party, so we do NOT also wait for every *connected* peer: a
            // peer that connected but then never signs (crash, refusal, or a
            // straggler) must not hold the gate open. Waiting on the connected
            // set is what hung the run in the connect-then-stall case.
            self.completed_peers.read().await.len() >= quorum
        } else {
            false
        };
        if ready {
            self.advance_step_if(cur).await;
        }
    }

    pub async fn store_peer_data(&self, peer_id: CantonId, data: Vec<u8>) {
        let mut peer_data = self.peer_data.write().await;
        peer_data.insert(peer_id, data);
    }

    pub async fn get_all_peer_data(&self) -> HashMap<CantonId, Vec<u8>> {
        self.peer_data.read().await.clone()
    }

    pub async fn clear_peer_data(&self) {
        let mut peer_data = self.peer_data.write().await;
        peer_data.clear();
    }

    pub async fn has_peer_completed(&self, peer_id: &CantonId) -> bool {
        let completed = self.completed_peers.read().await;
        completed.contains(peer_id)
    }

    pub async fn peer_connected(&self, peer_id: CantonId) {
        if !self.expected_peers.contains(&peer_id) {
            tracing::warn!(
                "ignoring connect from unexpected peer {peer_id} (not in expected set for {})",
                self.instance_name
            );
            return;
        }

        {
            let mut connected = self.connected_peers.write().await;
            if connected.insert(peer_id.clone()) {
                tracing::info!(
                    "Peer connected: {peer_id} ({}/{}, need {} to start)",
                    connected.len(),
                    self.expected_peers.len(),
                    self.peers_quorum()
                );
            }
        }
        // Re-evaluate on every poll, not just the first connect: peers keep
        // polling while they wait, so this re-checks a gate that became
        // satisfiable without a fresh event (e.g. a quorum restored after a
        // restart).
        self.maybe_advance().await;
    }

    pub async fn current_command(&self) -> Option<MessageType> {
        let step = self.current_step.read().await;
        step.to_command()
    }

    pub async fn peer_completed(&self, peer_id: CantonId) {
        if !self.expected_peers.contains(&peer_id) {
            tracing::warn!(
                "ignoring step completion from unexpected peer {peer_id} (not in expected set for {})",
                self.instance_name
            );
            return;
        }

        {
            let mut completed = self.completed_peers.write().await;
            completed.insert(peer_id.clone());
            let current = *self.current_step.read().await;
            let completed_vec: Vec<CantonId> = completed.iter().cloned().collect();
            tracing::info!(
                "Peer completed step {current:?}: {peer_id} ({}/{})",
                completed_vec.len(),
                self.expected_peers.len()
            );
            // Persist the new completed-peers set. Failures here are logged but
            // don't abort the workflow — on a future restart the recovery path
            // re-issues the command, which steps are designed to no-op when the
            // artefact already exists.
            self.persist_step_progress(current, completed_vec).await;
        }
        self.maybe_advance().await;
    }

    /// Unconditional advance, used by the coordinator loop which drives its own
    /// non-peer steps sequentially (one task, no self-race).
    pub async fn advance_step(&self) {
        let mut current = self.current_step.write().await;
        self.advance_locked(&mut current).await;
    }

    /// Advance only if the machine is still on `expected`. Peer events run on
    /// per-connection tasks, so with quorum < total two of them can observe the
    /// same gate open at once; without this check-then-act guard both would
    /// advance and skip a step. The CAS re-check under the write lock makes the
    /// second call a no-op. Also used by a coordinator loop to self-advance a
    /// peer gate that no peer event will ever trip (the 1-of-N solo case).
    pub async fn advance_step_if(&self, expected: S) {
        let mut current = self.current_step.write().await;
        if *current != expected {
            return;
        }
        self.advance_locked(&mut current).await;
    }

    /// Shared advance body; caller holds the `current_step` write lock.
    /// Does NOT flip status to Completed — the spawning task does that via
    /// `mark_run_completed` once `start_coordinator` returns (doing it here
    /// triggers the artifact cleanup before the post-workflow PARTY_ID read and
    /// re-marks the run Failed).
    async fn advance_locked(&self, current: &mut S) {
        let mut completed = self.completed_peers.write().await;
        if let Some(next_step) = current.next() {
            tracing::info!("Advancing workflow: {:?} -> {next_step:?}", *current);
            *current = next_step;
            completed.clear();
            self.persist_step_progress(next_step, Vec::new()).await;
        } else {
            tracing::info!("Workflow complete!");
        }
    }

    /// Mark the run as Failed with an error message. Used when a workflow
    /// step returns an error.
    pub async fn mark_failed(&self, error: impl Into<String>) {
        self.persist_status(WorkflowProgress::Failed, Some(error.into()))
            .await;
    }

    async fn persist_step_progress(&self, step: S, completed: Vec<CantonId>) {
        let updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let mut tx = match self.db.begin_transaction().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "persist_step_progress: begin_transaction failed for {}: {e}",
                    self.instance_name
                );
                return;
            }
        };
        if let Err(e) = tx
            .update_workflow_run_step(
                &self.instance_name,
                step.step_name(),
                step.step_index(),
                &completed,
                updated_at,
            )
            .await
        {
            tracing::warn!(
                "persist_step_progress: update failed for {}: {e}",
                self.instance_name
            );
            return;
        }
        if let Err(e) = Commitable::commit(tx).await {
            tracing::warn!(
                "persist_step_progress: commit failed for {}: {e}",
                self.instance_name
            );
        }
    }

    async fn persist_status(&self, status: WorkflowProgress, error: Option<String>) {
        let updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let mut tx = match self.db.begin_transaction().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "persist_status: begin_transaction failed for {}: {e}",
                    self.instance_name
                );
                return;
            }
        };
        if let Err(e) = tx
            .set_workflow_run_status(&self.instance_name, status, error.as_deref(), updated_at)
            .await
        {
            tracing::warn!(
                "persist_status: update failed for {}: {e}",
                self.instance_name
            );
            return;
        }
        if let Err(e) = Commitable::commit(tx).await {
            tracing::warn!(
                "persist_status: commit failed for {}: {e}",
                self.instance_name
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::canton_id::{NAMESPACE_LENGTH, Namespace};
    use crate::db::MIGRATOR;

    // `SqlitePool` and the workflow types come in via `use super::*` (the parent
    // module already imports `sqlx::SqlitePool`).
    use super::*;

    /// Minimal three-step workflow used to exercise the generic state machine.
    /// `Sign` is the only peer-gated step; `WaitPeers` is the connection-gated
    /// waiting step.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum TestStep {
        WaitPeers,
        Sign,
        Done,
    }

    impl WorkflowStep for TestStep {
        fn to_command(&self) -> Option<MessageType> {
            match self {
                TestStep::Sign => Some(MessageType::SignDns),
                _ => None,
            }
        }
        fn next(&self) -> Option<Self> {
            match self {
                TestStep::WaitPeers => Some(TestStep::Sign),
                TestStep::Sign => Some(TestStep::Done),
                TestStep::Done => None,
            }
        }
        fn requires_peers(&self) -> bool {
            matches!(self, TestStep::Sign)
        }
        fn is_waiting_for_peers(&self) -> bool {
            matches!(self, TestStep::WaitPeers)
        }
        fn step_index(&self) -> i64 {
            match self {
                TestStep::WaitPeers => 0,
                TestStep::Sign => 1,
                TestStep::Done => 2,
            }
        }
        fn step_total() -> i64 {
            3
        }
        fn step_name(&self) -> &'static str {
            match self {
                TestStep::WaitPeers => "WaitPeers",
                TestStep::Sign => "Sign",
                TestStep::Done => "Done",
            }
        }
        fn try_from_step_name(name: &str) -> Option<Self> {
            match name {
                "WaitPeers" => Some(TestStep::WaitPeers),
                "Sign" => Some(TestStep::Sign),
                "Done" => Some(TestStep::Done),
                _ => None,
            }
        }
        fn kind() -> WorkflowKind {
            WorkflowKind::Onboarding
        }
    }

    /// Build a distinct, deterministic peer id from a single byte.
    fn peer(n: u8) -> CantonId {
        CantonId::new(format!("p{n}"), Namespace::new([n; NAMESPACE_LENGTH]))
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn peer_completed_advances_only_on_last_expected_peer(pool: SqlitePool) {
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::Sign,
            vec![peer(1), peer(2)],
            None,
        );

        state.peer_completed(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::Sign);

        state.peer_completed(peer(2)).await;
        assert_eq!(state.current_step().await, TestStep::Done);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn peer_completed_does_not_advance_on_non_requires_peers_step(pool: SqlitePool) {
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::WaitPeers,
            vec![peer(1)],
            None,
        );

        // WaitPeers does not require peers, so a completion must not advance it.
        state.peer_completed(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::WaitPeers);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn peer_connected_advances_from_waiting_on_last_peer(pool: SqlitePool) {
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::WaitPeers,
            vec![peer(1), peer(2)],
            None,
        );

        state.peer_connected(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::WaitPeers);

        // Duplicate connect from the same peer is deduped and must not count.
        state.peer_connected(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::WaitPeers);

        state.peer_connected(peer(2)).await;
        assert_eq!(state.current_step().await, TestStep::Sign);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn peer_connected_does_not_advance_when_not_waiting(pool: SqlitePool) {
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::Sign,
            vec![peer(1)],
            None,
        );

        // Sign is not a waiting step, so a connect must not advance it.
        state.peer_connected(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::Sign);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn ignores_completion_from_unexpected_peer(pool: SqlitePool) {
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::Sign,
            vec![peer(1), peer(2)],
            None,
        );

        state.peer_completed(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::Sign);

        // peer(9) is not in the expected set. Without the guard its insert would
        // make the completed count 2 == 2 and wrongly advance to Done.
        state.peer_completed(peer(9)).await;
        assert_eq!(state.current_step().await, TestStep::Sign);

        // The last genuinely-expected peer still advances the workflow.
        state.peer_completed(peer(2)).await;
        assert_eq!(state.current_step().await, TestStep::Done);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn ignores_connect_from_unexpected_peer(pool: SqlitePool) {
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::WaitPeers,
            vec![peer(1), peer(2)],
            None,
        );

        state.peer_connected(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::WaitPeers);

        // peer(9) is not expected; it must neither count nor advance.
        state.peer_connected(peer(9)).await;
        assert_eq!(state.current_step().await, TestStep::WaitPeers);

        state.peer_connected(peer(2)).await;
        assert_eq!(state.current_step().await, TestStep::Sign);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn from_persisted_resumes_without_losing_progress(pool: SqlitePool) {
        // Resume mid-Sign with peer(1) already completed; only peer(2) remains.
        let state = WorkflowState::from_persisted(
            pool,
            "test-run".to_string(),
            TestStep::Sign,
            vec![peer(1), peer(2)],
            vec![peer(1)],
            None,
        );

        state.peer_completed(peer(2)).await;
        assert_eq!(state.current_step().await, TestStep::Done);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn advance_step_at_terminal_is_noop(pool: SqlitePool) {
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::Done,
            vec![peer(1)],
            None,
        );

        // Done has no successor, so advancing is a no-op.
        state.advance_step().await;
        assert_eq!(state.current_step().await, TestStep::Done);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn peer_connected_advances_at_threshold(pool: SqlitePool) {
        // quorum 1 of 2: the first connect starts the workflow.
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::WaitPeers,
            vec![peer(1), peer(2)],
            Some(1),
        );

        state.peer_connected(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::Sign);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn signing_gate_advances_at_quorum_despite_connected_non_signer(pool: SqlitePool) {
        // Regression (#207): quorum 1, both peers connect, but peer(2) never
        // signs (connect-then-stall — crash or refusal). The run must still
        // advance once the quorum (peer(1)) has signed; a connected-but-silent
        // peer must not hold the gate open. M signatures satisfy an M-of-N party.
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::Sign,
            vec![peer(1), peer(2)],
            Some(1),
        );

        state.peer_connected(peer(1)).await;
        state.peer_connected(peer(2)).await; // connects, then stalls forever

        state.peer_completed(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::Done);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn signing_gate_reevaluated_on_poll_after_restart(pool: SqlitePool) {
        // Regression (#207): the quorum was met before a restart (peer(1)
        // completed) but the advance never happened. On resume no fresh
        // completion arrives — the completed peer only re-polls — so the gate
        // must be re-evaluated on that poll rather than hanging.
        let state = WorkflowState::from_persisted(
            pool,
            "test-run".to_string(),
            TestStep::Sign,
            vec![peer(1), peer(2)],
            vec![peer(1)], // restored: peer(1) already completed pre-restart
            Some(1),
        );

        // A bare poll (no new completion) must trip the already-met gate.
        state.peer_connected(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::Done);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn signing_gate_skips_absent_peer_at_quorum(pool: SqlitePool) {
        // quorum 1, peer(2) never connects → peer(1) signing is enough.
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::Sign,
            vec![peer(1), peer(2)],
            Some(1),
        );

        state.peer_connected(peer(1)).await;

        state.peer_completed(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::Done);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn require_all_signing_gate_unchanged_with_connections(pool: SqlitePool) {
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::Sign,
            vec![peer(1), peer(2)],
            None,
        );

        state.peer_connected(peer(1)).await;
        state.peer_connected(peer(2)).await;

        state.peer_completed(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::Sign);

        state.peer_completed(peer(2)).await;
        assert_eq!(state.current_step().await, TestStep::Done);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn start_threshold_above_peer_count_clamps_to_all(pool: SqlitePool) {
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::WaitPeers,
            vec![peer(1), peer(2)],
            Some(5),
        );

        state.peer_connected(peer(1)).await;
        assert_eq!(state.current_step().await, TestStep::WaitPeers);

        state.peer_connected(peer(2)).await;
        assert_eq!(state.current_step().await, TestStep::Sign);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn advance_step_if_only_advances_from_expected(pool: SqlitePool) {
        let state = WorkflowState::new(
            pool,
            "test-run".to_string(),
            TestStep::WaitPeers,
            vec![peer(1)],
            None,
        );

        // Stale "from" (a step we're already past / not on): no-op. This is the
        // CAS guard that stops two concurrent peer events from double-advancing.
        state.advance_step_if(TestStep::Sign).await;
        assert_eq!(state.current_step().await, TestStep::WaitPeers);

        // Matching "from": advances exactly one step.
        state.advance_step_if(TestStep::WaitPeers).await;
        assert_eq!(state.current_step().await, TestStep::Sign);

        // A second call with the now-stale "from" must not advance again.
        state.advance_step_if(TestStep::WaitPeers).await;
        assert_eq!(state.current_step().await, TestStep::Sign);
    }
}
