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
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            instance_name,
            current_step: RwLock::new(initial_step),
            expected_peers: expected_peers.into_iter().collect(),
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
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            instance_name,
            current_step: RwLock::new(current_step),
            expected_peers: expected_peers.into_iter().collect(),
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
        // Guard against non-expected peers. The auto-advance gate below uses
        // `expected_peers.len()` as the total, so counting a peer that isn't in
        // the expected set (stale, duplicate, or re-onboarded) could trip the
        // gate without all expected peers having acted — advancing the workflow
        // with a missing signature. Expected peers behave exactly as before.
        if !self.expected_peers.contains(&peer_id) {
            tracing::warn!(
                "ignoring connect from unexpected peer {peer_id} (not in expected set for {})",
                self.instance_name
            );
            return;
        }

        let mut connected = self.connected_peers.write().await;

        let is_new = connected.insert(peer_id.clone());
        if !is_new {
            return;
        }

        let connected_count = connected.len();
        let total_count = self.expected_peers.len();
        tracing::info!("Peer connected: {peer_id} ({connected_count}/{total_count})");

        if connected_count == total_count {
            let current = self.current_step.read().await;
            if current.is_waiting_for_peers() {
                drop(current);
                drop(connected);
                self.advance_step().await;
            }
        }
    }

    pub async fn current_command(&self) -> Option<MessageType> {
        let step = self.current_step.read().await;
        step.to_command()
    }

    pub async fn peer_completed(&self, peer_id: CantonId) {
        // Guard against non-expected peers. The auto-advance gate below uses
        // `expected_peers.len()` as the total, so counting a peer that isn't in
        // the expected set (stale, duplicate, or re-onboarded) could trip the
        // gate without all expected peers having acted — advancing the workflow
        // with a missing signature. Expected peers behave exactly as before.
        if !self.expected_peers.contains(&peer_id) {
            tracing::warn!(
                "ignoring step completion from unexpected peer {peer_id} (not in expected set for {})",
                self.instance_name
            );
            return;
        }

        let mut completed = self.completed_peers.write().await;
        completed.insert(peer_id.clone());

        let current = self.current_step.read().await;
        let completed_count = completed.len();
        let total_count = self.expected_peers.len();
        let step_name = format!("{current:?}");
        tracing::info!(
            "Peer completed step {step_name}: {peer_id} ({completed_count}/{total_count})"
        );

        // Persist the new completed-peers set. Failures here are logged
        // but don't abort the workflow — on a future restart the recovery path
        // would just re-issue the command, which steps are designed to no-op
        // when the artefact already exists.
        let completed_vec: Vec<CantonId> = completed.iter().cloned().collect();
        self.persist_step_progress(*current, completed_vec).await;

        if current.requires_peers() && completed_count == total_count {
            drop(current);
            drop(completed);
            self.advance_step().await;
        }
    }

    pub async fn advance_step(&self) {
        let mut current = self.current_step.write().await;
        let mut completed = self.completed_peers.write().await;

        if let Some(next_step) = current.next() {
            let current_name = format!("{current:?}");
            let next_name = format!("{next_step:?}");
            tracing::info!("Advancing workflow: {current_name} -> {next_name}");
            *current = next_step;
            completed.clear();
            self.persist_step_progress(next_step, Vec::new()).await;
        } else {
            tracing::info!("Workflow complete!");
        }
        // Do NOT flip status to Completed here — the spawning task does that
        // via `mark_run_completed` once `start_coordinator` returns. Doing it
        // inside the state machine triggers the workflow_artifacts cleanup
        // before the post-workflow PARTY_ID read (onboarding) and re-marks
        // the run as Failed.
    }

    /// Mark the run as Failed with an error message. Used when a workflow
    /// step returns an error.
    pub async fn mark_failed(&self, error: impl Into<String>) {
        self.persist_status(WorkflowProgress::Failed, Some(error.into()))
            .await;
    }

    /// Mark the run as Cancelled. Used by the cancel propagation path on
    /// peers when they receive a `CancelWorkflow` Noise message.
    pub async fn mark_cancelled(&self) {
        self.persist_status(WorkflowProgress::Cancelled, None).await;
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
        let state = WorkflowState::new(pool, "test-run".to_string(), TestStep::Sign, vec![peer(1)]);

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
        );

        state.peer_completed(peer(2)).await;
        assert_eq!(state.current_step().await, TestStep::Done);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn advance_step_at_terminal_is_noop(pool: SqlitePool) {
        let state = WorkflowState::new(pool, "test-run".to_string(), TestStep::Done, vec![peer(1)]);

        // Done has no successor, so advancing is a no-op.
        state.advance_step().await;
        assert_eq!(state.current_step().await, TestStep::Done);
    }
}
