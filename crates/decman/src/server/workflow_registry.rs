//! The registry of running workflows: per-run state, the guard that removes a run, and peer jobs.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock as StdRwLock},
};

use common::types::{WorkflowKind, WorkflowProgress, WorkflowRole};
use tokio::sync::RwLock;

use crate::{canton_id::CantonId, noise::server::ActiveWorkflow};

/// Trait for workflow status types that can be used with HttpWorkflowState
pub trait WorkflowStatus: Default + Copy + Send + Sync {}

/// Generic state for tracking HTTP-triggered workflows. Holds enough context
/// for the matching `/cancel` endpoint to abort the spawn and notify the
/// peers that received an invite.
pub struct HttpWorkflowState<S: WorkflowStatus> {
    pub status: RwLock<S>,
    pub error: RwLock<Option<String>>,
    pub abort_handle: tokio::sync::Mutex<Option<tokio::task::AbortHandle>>,
    pub invited_peers: RwLock<Vec<CantonId>>,
}

impl<S: WorkflowStatus> Default for HttpWorkflowState<S> {
    fn default() -> Self {
        Self {
            status: RwLock::new(S::default()),
            error: RwLock::new(None),
            abort_handle: tokio::sync::Mutex::new(None),
            invited_peers: RwLock::new(Vec::new()),
        }
    }
}

impl<S: WorkflowStatus> HttpWorkflowState<S> {
    pub fn new() -> Self {
        Self::default()
    }
}

/// One in-flight workflow this node owns (coordinator- or peer-side),
/// type-erased over kind so a single [`WorkflowRegistry`] + a single
/// [`PeerJob`] queue hold every concurrent run regardless of kind. Keyed in
/// the registry by `instance_name`.
pub struct WorkflowInstance {
    pub instance_name: String,
    pub kind: WorkflowKind,
    pub role: WorkflowRole,
    /// HTTP-facing state (status, error, abort handle, invitees) the
    /// `/workflows/{instance_name}/{status,cancel}` endpoints read and mutate.
    /// Uniform across kinds — every kind's status collapses to
    /// [`WorkflowProgress`].
    pub http: Arc<HttpWorkflowState<WorkflowProgress>>,
    /// Coordinator-only Noise handle the always-on listener routes a peer's
    /// workflow-command traffic to. `None` for peer-side runs (peers connect
    /// outbound as clients and are never routed to here). `std::sync::RwLock`
    /// so the listener clones the handle out without awaiting.
    pub active: StdRwLock<Option<ActiveWorkflow>>,
}

impl WorkflowInstance {
    /// Build a fresh instance with empty HTTP state and no Noise handle yet.
    pub fn new(instance_name: String, kind: WorkflowKind, role: WorkflowRole) -> Arc<Self> {
        Arc::new(Self {
            instance_name,
            kind,
            role,
            http: Arc::new(HttpWorkflowState::new()),
            active: StdRwLock::new(None),
        })
    }

    /// Register the coordinator's typed Noise server so the always-on listener
    /// can route this run's commands to it.
    pub fn set_active(&self, workflow: ActiveWorkflow) {
        *self.active.write().unwrap_or_else(|e| e.into_inner()) = Some(workflow);
    }
}

/// Instance-keyed registry of every in-flight workflow this node owns. Replaces
/// the single-tenant per-kind `HttpWorkflowState` singletons, the global
/// in-flight gate, and the single `active_workflow` routing slot: any number of
/// workflows (even of the same kind) run side-by-side, each addressed by
/// `instance_name`. The always-on Noise listener routes a peer's command via
/// [`route`](Self::route) using `Message::instance`.
///
/// Uses `std::sync::RwLock` (not tokio) so the listener holds the lock only
/// long enough to clone a handle out — never across an await. The inner
/// `HttpWorkflowState` locks are tokio and are awaited only after the `Arc`
/// has been cloned out and the registry lock released.
#[derive(Clone, Default)]
pub struct WorkflowRegistry {
    inner: Arc<StdRwLock<HashMap<String, Arc<WorkflowInstance>>>>,
}

impl WorkflowRegistry {
    /// Build an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a freshly-built instance. Returns `false` (and inserts nothing)
    /// if a run is already registered under that `instance_name` — the caller
    /// turns that into a 409.
    pub fn insert(&self, instance: Arc<WorkflowInstance>) -> bool {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if guard.contains_key(&instance.instance_name) {
            return false;
        }
        guard.insert(instance.instance_name.clone(), instance);
        true
    }

    /// Look up the instance registered under `instance_name`.
    pub fn get(&self, instance_name: &str) -> Option<Arc<WorkflowInstance>> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(instance_name)
            .cloned()
    }

    /// Remove and return the instance for `instance_name`. Called once a run
    /// reaches a terminal status so the registry doesn't accumulate stale
    /// entries (see also [`WorkflowGuard`], which does this on drop).
    pub fn remove(&self, instance_name: &str) -> Option<Arc<WorkflowInstance>> {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(instance_name)
    }

    /// Clone out the coordinator Noise handle to route a workflow command to,
    /// without awaiting.
    ///
    /// When `instance_name` is non-empty (the peer learned the coordinator's run
    /// from the invite and stamps every command with it), route by **exact
    /// match only**. If that run isn't registered, or hasn't set its Noise
    /// handle yet (coordinator still spinning up), return `None` — the listener
    /// replies 503 and the peer's bounded retry waits for it. Never fall back to
    /// a *different* run for a peer that named its own: in rapid start/restart
    /// sequences a sole-active fallback would hand the command to a
    /// stale/completing workflow, which `Disconnect`s the peer and ends its run
    /// with no work done.
    ///
    /// Only an empty key — a peer that predates instance routing, or a resumed
    /// peer with no stored coordinator instance — falls back to the sole active
    /// run (if exactly one), since it has no key to match on. Note the
    /// fallback's inherent imprecision: if the no-key peer's true run is
    /// registered-but-not-yet-active while a sibling IS active, it gets routed
    /// to the sibling. Post-upgrade only pre-migration resumed rows produce
    /// no-key traffic (the wire break forces lockstep upgrades), so the
    /// exposure is one resume window per legacy row.
    pub fn route(&self, instance_name: &str) -> Option<ActiveWorkflow> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        if !instance_name.is_empty() {
            return guard
                .get(instance_name)
                .and_then(|i| i.active.read().unwrap_or_else(|e| e.into_inner()).clone());
        }
        // Empty key: fall back to the sole active run, if exactly one.
        let mut actives = guard
            .values()
            .filter_map(|i| i.active.read().unwrap_or_else(|e| e.into_inner()).clone());
        let first = actives.next();
        match actives.next() {
            Some(_) => None,
            None => first,
        }
    }

    /// Snapshot every registered instance.
    pub fn snapshot(&self) -> Vec<Arc<WorkflowInstance>> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }
}

/// Removes a [`WorkflowInstance`] from the [`WorkflowRegistry`] on drop —
/// including on coordinator/peer task panic or abort — so a finished or
/// cancelled run can never linger and misroute later commands. Removing by the
/// guard's own `instance_name` means one finishing run never deregisters a
/// different concurrent run.
pub struct WorkflowGuard {
    registry: WorkflowRegistry,
    instance_name: String,
}

impl WorkflowGuard {
    /// Tie `instance_name`'s registry entry to the returned guard's lifetime.
    pub fn new(registry: WorkflowRegistry, instance_name: String) -> Self {
        Self {
            registry,
            instance_name,
        }
    }
}

impl Drop for WorkflowGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.instance_name);
    }
}

/// A peer-side workflow job: emitted by `accept_invitation` / the
/// `RetryWorkflow` listener arm onto the single `mpsc::UnboundedSender` and
/// consumed by the peer listener, which spawns `workflow::start_peer` for it.
/// Carrying `kind`, `instance_name`, and `coordinator_pubkey` on the message
/// means concurrent accepts of any kind no longer race over a global slot.
#[derive(Clone, Debug)]
pub struct PeerJob {
    pub kind: WorkflowKind,
    /// The peer-side `workflow_runs` row primary key (local synthetic name).
    pub instance_name: String,
    /// The coordinator's own run `instance_name`, taken from the invite's
    /// `workflow_instance`. The peer tags every workflow command with it
    /// (`Message::instance`) so the coordinator's always-on listener routes the
    /// command to the right concurrent run. Empty if the invite predated
    /// instance routing — the coordinator then falls back to its sole run.
    pub coordinator_instance: String,
    pub coordinator_pubkey: String,
}

// `WorkflowProgress` is now defined in `common::types` and re-exported above.
// `WorkflowStatus` is a backend-local trait, so this impl on the (now foreign)
// `WorkflowProgress` is permitted by the orphan rule.
impl WorkflowStatus for WorkflowProgress {}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::{
        config::{NetworkConfig, NodeConfig},
        db::MIGRATOR,
        error::Result,
        noise::{load_or_generate_keypair, server::NoiseServer},
        server::peer_status::LastSeen,
        workflow::OnboardingStep,
    };

    /// Build a real `ActiveWorkflow` (the enum is over `NoiseServer<S>`, which
    /// has no test double) for registry routing tests. `dir` must outlive the
    /// call — `NoiseServer::new` reads the keypair from it.
    async fn test_active_workflow(
        pool: &SqlitePool,
        dir: &tempfile::TempDir,
        instance: &str,
    ) -> Result<ActiveWorkflow> {
        let config = NodeConfig::default().with_root_dir(dir.path());
        tokio::fs::create_dir_all(config.data_dir()).await?;
        load_or_generate_keypair(config.key_file_path()).await?;
        let last_seen: LastSeen = Arc::new(RwLock::new(HashMap::new()));
        let server = NoiseServer::new(
            config,
            NetworkConfig::from_peers(Vec::new()),
            pool.clone(),
            instance.to_string(),
            OnboardingStep::WaitingForPeers,
            None,
            last_seen,
        )
        .await
        .map_err(|e| anyhow::anyhow!("NoiseServer::new: {e}"))?;
        Ok(ActiveWorkflow::Onboarding(Arc::new(server)))
    }

    #[test]
    fn registry_rejects_duplicate_instance_and_guard_removes_own_entry() {
        let registry = WorkflowRegistry::new();
        let a = WorkflowInstance::new(
            "a-creation".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );
        let a_dup = WorkflowInstance::new(
            "a-creation".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );
        let b = WorkflowInstance::new(
            "b-creation".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );

        assert!(registry.insert(a));
        // Same-instance registration must be rejected (the start handlers turn
        // this into a 409) while a distinct sibling registers fine.
        assert!(!registry.insert(a_dup));
        assert!(registry.insert(b));

        // A guard dropping removes exactly its own entry — never a sibling's.
        let guard = WorkflowGuard::new(registry.clone(), "a-creation".to_string());
        drop(guard);
        assert!(registry.get("a-creation").is_none());
        assert!(registry.get("b-creation").is_some());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn route_is_exact_for_keyed_peers_with_sole_active_fallback_for_legacy(
        pool: SqlitePool,
    ) -> Result {
        let dir = tempfile::tempdir()?;
        let registry = WorkflowRegistry::new();

        // Run A is fully live (registered + Noise handle set); run B is
        // registered but still spinning up (no handle yet) — the exact window
        // the routing rules exist for.
        let a = WorkflowInstance::new(
            "a-creation".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );
        let b = WorkflowInstance::new(
            "b-creation".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );
        assert!(registry.insert(a.clone()));
        assert!(registry.insert(b.clone()));
        a.set_active(test_active_workflow(&pool, &dir, "a-creation").await?);

        // Keyed peers route exactly: A's traffic reaches A.
        assert!(registry.route("a-creation").is_some());
        // A peer naming run B must get None (503 -> bounded retry) while B
        // spins up — NEVER a fallback onto sibling A, which would Disconnect
        // it (the G3 regression this rule fixed).
        assert!(registry.route("b-creation").is_none());
        // An unknown key (cancelled/dismissed run) also gets None.
        assert!(registry.route("no-such-run").is_none());
        // A legacy/resumed peer with no key falls back to the sole active run.
        assert!(registry.route("").is_some());

        // Once B is live too, exact keys both route, but an empty key is
        // ambiguous and must refuse rather than guess.
        b.set_active(test_active_workflow(&pool, &dir, "b-creation").await?);
        assert!(registry.route("a-creation").is_some());
        assert!(registry.route("b-creation").is_some());
        assert!(registry.route("").is_none());

        Ok(())
    }
}
