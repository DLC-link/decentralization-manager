use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    sync::Arc,
    time::Instant,
};

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use super::threshold::resolve_peer_threshold;
use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig, Peer},
    db::schema::SchemaRead,
    noise::{NoiseError, NoiseKeypair},
    server::{WorkflowProgress, peer_status::LastSeen},
    workflow::{WorkflowState, state::WorkflowStep},
};

/// Coordinator server that accepts connections from peers
pub struct NoiseServer<S: WorkflowStep + 'static> {
    pub(super) node_config: Arc<NodeConfig>,
    pub(super) keypair: Arc<NoiseKeypair>,
    /// Full peer records (address + port + public_key) kept around so the
    /// server can fan out broadcast notifications (e.g. CancelInvite after
    /// a DeclineInvitation) without going through the DB on the hot path.
    pub(super) peers: Vec<Peer>,
    pub(super) workflow_state: Arc<WorkflowState<S>>,
    pub(super) last_seen: LastSeen,
    /// Peers that reported giving up on this run, with why.
    pub(super) departed: RwLock<HashMap<CantonId, String>>,
    /// When this process began serving the run. A peer not heard from since
    /// counts as silent from here, so a restart does not count it out at once.
    pub(super) serving_since: Instant,
    pub(super) _p: PhantomData<S>,
}

impl<S: WorkflowStep + 'static> NoiseServer<S> {
    /// Create a new Noise server
    ///
    /// # Arguments
    /// * `node_config` - Node configuration
    /// * `network_config` - Network configuration
    /// * `db` - SQLite pool used to persist `WorkflowState` updates against the
    ///   matching `workflow_runs` row.
    /// * `instance_name` - Identifier for the persisted run (matches the row's
    ///   primary key).
    /// * `initial_step` - Initial workflow step
    /// * `exclude_participants` - Optional list of participant IDs to exclude from peers (e.g., participants being kicked)
    ///
    /// # Errors
    ///
    /// Returns an error if this node's Noise key file cannot be loaded. A failed
    /// read of the persisted run does not fail; it falls back to the configured
    /// peers.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        node_config: NodeConfig,
        network_config: NetworkConfig,
        db: SqlitePool,
        instance_name: String,
        initial_step: S,
        exclude_participants: Option<Vec<String>>,
        last_seen: LastSeen,
    ) -> Result<Self, NoiseError> {
        let keypair = NoiseKeypair::from_file(&node_config.key_file_path()).await?;

        let excluded: HashSet<String> = exclude_participants
            .unwrap_or_default()
            .into_iter()
            .collect();

        if !excluded.is_empty() {
            tracing::info!(
                "Excluding {count} participant(s) from peers: {participants}",
                count = excluded.len(),
                participants = excluded.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        }

        // Look up the persisted run up-front. Its `expected_peers` is the
        // authoritative invitee set the start handler selected for *this* run,
        // which can be a strict subset of the configured mesh — onboarding and
        // dars both invite a chosen subset of the peers this node knows about.
        // Deriving the wait set from the full configured mesh instead made the
        // coordinator wait in `WaitingForPeers` for peers it was never going to
        // invite and that can never connect, so any party smaller than the full
        // mesh (e.g. 1 coordinator + 1 peer) hung forever.
        // Prefer the persisted invitees; the coordinator always
        // inserts its row before constructing this server, so the
        // configured-mesh fallback only covers the (unexpected) no-row case.
        let persisted = match SchemaRead::get_workflow_run(&db, &instance_name).await {
            Ok(run) => run,
            Err(e) => {
                tracing::warn!(
                    "Failed to look up persisted workflow_runs row for {instance_name}: {e}; \
                     falling back to configured peers"
                );
                None
            }
        };

        let expected_peers: Vec<CantonId> = match persisted.as_ref() {
            Some(run) => run.expected_peers.clone(),
            None => network_config
                .peers
                .iter()
                .filter(|p| {
                    p.participant_id != *node_config.participant_id()
                        && !excluded.contains(&p.participant_id.to_string())
                })
                .map(|p| p.participant_id.clone())
                .collect(),
        };

        tracing::info!(
            "Expected {count} peer(s): {peers}",
            count = expected_peers.len(),
            peers = expected_peers
                .iter()
                .map(CantonId::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Scope the fan-out peer records to this run's participant set so
        // best-effort broadcasts (e.g. CancelInvite on decline) only reach the
        // peers actually invited to this run, never unrelated configured peers.
        let expected_set: HashSet<CantonId> = expected_peers.iter().cloned().collect();
        let peers = network_config
            .peers
            .iter()
            .filter(|p| expected_set.contains(&p.participant_id))
            .cloned()
            .collect();

        let peer_threshold = resolve_peer_threshold(
            S::kind(),
            &node_config,
            persisted.as_ref().and_then(|run| run.dec_party_id.as_ref()),
            &db,
            expected_peers.len(),
        )
        .await;

        // Resume-aware construction: if an InProgress workflow_runs row already
        // exists for `instance_name` (we restarted mid-flight), re-hydrate the
        // state machine from its persisted `current_step` + already-completed
        // peers instead of starting fresh from `initial_step`. The
        // coordinator workflow loop is naturally driven by `current_step`, so
        // seeding it from the row picks the run back up at the right place.
        let workflow_state = match persisted {
            Some(run) if run.status == WorkflowProgress::InProgress => {
                match S::try_from_step_name(&run.current_step) {
                    Some(step) => {
                        tracing::info!(
                            "Resuming workflow {instance_name} at step {step:?} \
                             ({completed} of {expected} peers completed)",
                            completed = run.completed_peers.len(),
                            expected = expected_peers.len()
                        );
                        WorkflowState::from_persisted(
                            db,
                            instance_name,
                            step,
                            expected_peers,
                            run.completed_peers,
                            peer_threshold,
                        )
                    }
                    None => {
                        tracing::warn!(
                            "Persisted current_step {current_step:?} for {instance_name} is not a valid \
                             {kind} step; starting fresh from {initial_step:?}",
                            current_step = run.current_step,
                            kind = std::any::type_name::<S>()
                        );
                        WorkflowState::new(
                            db,
                            instance_name,
                            initial_step,
                            expected_peers,
                            peer_threshold,
                        )
                    }
                }
            }
            _ => WorkflowState::new(
                db,
                instance_name,
                initial_step,
                expected_peers,
                peer_threshold,
            ),
        };

        Ok(Self {
            node_config: Arc::new(node_config),
            keypair: Arc::new(keypair),
            peers,
            workflow_state,
            last_seen,
            departed: RwLock::new(HashMap::new()),
            serving_since: Instant::now(),
            _p: PhantomData,
        })
    }

    pub fn get_workflow_state(&self) -> Arc<WorkflowState<S>> {
        self.workflow_state.clone()
    }
}
