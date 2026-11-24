//! Boot recovery: respawning the workflow runs a shutdown interrupted.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::SqlitePool;
use tokio::sync::{RwLock, mpsc};

use crate::{
    auth::WorkflowAuth,
    config::NodeConfig,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    server::{
        PeerJob, WorkflowInstance, WorkflowRegistry, handlers,
        handlers::{
            CoordinatorRequest, NodeHandles, RunEnd, StatusOrder, TaskStart, run_log,
            spawn_coordinator_task,
        },
        peer_status::LastSeen,
        types::{WorkflowProgress, WorkflowRole, WorkflowRun},
    },
};

/// On boot, re-spawn any InProgress workflow runs that were interrupted by the
/// last shutdown. The previous task handle died with the process, but the
/// state machine + artefacts survived in SQLite, so we can pick the run back
/// up at its persisted `current_step`.
///
/// Coordinator-side: each run is rebuilt as its own [`WorkflowInstance`] in the
/// registry (keyed by `instance_name`) and its `start_coordinator` task is
/// re-spawned. Because every coordinator run is now routed independently by the
/// always-on listener via `Message::instance`, all of them resume concurrently
/// — not just the newest, as the former single-slot model required.
///
/// Peer-side: a [`PeerJob`] is re-enqueued so the peer listener re-spins
/// `start_peer`. Limitation (unchanged): the peer pulls its instance_name out
/// of the GenerateKeys / SignSubmissions / SignKick command payload — if the
/// coordinator is past those steps the peer cannot rebind and the run surfaces
/// as Failed for the operator to dismiss.
pub(super) async fn recover_in_progress_workflows(
    db: SqlitePool,
    config: NodeConfig,
    workflows: WorkflowRegistry,
    peer_job_sender: mpsc::UnboundedSender<PeerJob>,
    auth: Arc<RwLock<Option<WorkflowAuth>>>,
    last_seen: LastSeen,
) {
    let runs = match SchemaRead::get_in_progress_workflow_runs(&db).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("recover_in_progress_workflows: load failed: {e}");
            return;
        }
    };
    if runs.is_empty() {
        return;
    }
    tracing::info!(
        "Recovering {} in-progress workflow run(s) interrupted by last shutdown",
        runs.len()
    );

    for run in &runs {
        match run.role {
            WorkflowRole::Coordinator => {
                respawn_coordinator(
                    db.clone(),
                    config.clone(),
                    run,
                    workflows.clone(),
                    auth.clone(),
                    last_seen.clone(),
                )
                .await;
            }
            WorkflowRole::Peer => refire_peer(run, &peer_job_sender),
        }
    }
}

/// Re-spawn a coordinator-side workflow that was running when the node stopped.
/// The original `workflow_runs` row stays in place; the spawned task uses
/// `WorkflowState::from_persisted` (via `NoiseServer::new`) to resume at
/// `current_step` instead of restarting from `WaitingForPeers`. A fresh
/// [`WorkflowInstance`] is registered so the always-on listener can route this
/// run's commands and `/workflows/{instance}/cancel` can abort it.
pub(crate) async fn respawn_coordinator(
    db: SqlitePool,
    config: NodeConfig,
    run: &WorkflowRun,
    workflows: WorkflowRegistry,
    auth: Arc<RwLock<Option<WorkflowAuth>>>,
    last_seen: LastSeen,
) {
    let instance_name = run.instance_name.clone();
    let kind = run.kind;
    tracing::info!(
        "Resuming {kind:?} coordinator run {instance_name} at step {} \
         ({} of {} peers completed)",
        run.current_step,
        run.completed_peers.len(),
        run.expected_peers.len()
    );

    // Parse the persisted config for this kind into its request.
    let request = match CoordinatorRequest::from_persisted(kind, &run.config_json) {
        Ok(request) => request,
        Err(e) => {
            tracing::warn!(
                "respawn_coordinator: bad {kind:?} config_json for {instance_name}: {e}"
            );
            handlers::mark_run_failed(&db, kind, &instance_name, "Resume failed: invalid config")
                .await;
            return;
        }
    };

    // Recovery-window guard: this row was loaded as InProgress, but between then
    // and registering the in-memory instance a per-kind cancel could have hit
    // the persisted-row cancel path (`cancel_persisted_run`, reached only while
    // no instance is registered) and flipped it to Cancelled. Re-read the status
    // and skip resuming if it's no longer InProgress, so we don't resurrect a run
    // the operator just cancelled. (The initial-start path is already safe:
    // `register_and_persist` registers the instance before it persists the row.)
    match SchemaRead::get_workflow_run(&db, &instance_name).await {
        Ok(Some(r)) if r.status == WorkflowProgress::InProgress => {}
        Ok(Some(r)) => {
            tracing::info!(
                "respawn_coordinator: {instance_name} is now {:?}, not resuming",
                r.status
            );
            return;
        }
        Ok(None) => {
            tracing::warn!("respawn_coordinator: {instance_name} row vanished; not resuming");
            return;
        }
        Err(e) => {
            tracing::warn!(
                "respawn_coordinator: status re-check failed for {instance_name}: {e}; resuming anyway"
            );
        }
    }

    let instance = WorkflowInstance::new(instance_name.clone(), kind, WorkflowRole::Coordinator);
    *instance.http.invited_peers.write().await = run.expected_peers.clone();
    *instance.http.status.write().await = WorkflowProgress::InProgress;
    if !workflows.insert(instance.clone()) {
        tracing::warn!("respawn_coordinator: {instance_name} already registered; skipping");
        return;
    }

    let auth_snapshot = auth.read().await.clone();
    let handle = spawn_coordinator_task(
        NodeHandles {
            config,
            db,
            workflows,
            last_seen,
        },
        instance.clone(),
        request,
        auth_snapshot,
        TaskStart::Resume,
        RunEnd {
            order: StatusOrder::MemoryFirst,
            log: run_log!(),
            after_success: None,
        },
    );
    *instance.http.abort_handle.lock().await = Some(handle.abort_handle());
}

/// Re-enqueue a peer-side run as a [`PeerJob`] so the peer listener re-spins
/// `start_peer` against the persisted coordinator pubkey.
pub(crate) fn refire_peer(run: &WorkflowRun, peer_job_sender: &mpsc::UnboundedSender<PeerJob>) {
    let Some(pk) = run.coordinator_pubkey.clone() else {
        tracing::warn!(
            "Skipping peer recover for {}: no coordinator_pubkey persisted",
            run.instance_name
        );
        return;
    };
    let job = PeerJob {
        kind: run.kind,
        instance_name: run.instance_name.clone(),
        // Persisted at accept time (the invite's `workflow_instance`), so a
        // resumed peer routes its commands to the exact coordinator run instead
        // of relying on the sole-active fallback. Empty only for rows that
        // predate instance routing.
        coordinator_instance: run.coordinator_instance.clone().unwrap_or_default(),
        coordinator_pubkey: pk,
    };
    if peer_job_sender.send(job).is_err() {
        tracing::warn!(
            "Failed to re-enqueue {:?} peer job for resumed run {}: receiver dropped",
            run.kind,
            run.instance_name
        );
    } else {
        tracing::info!(
            "Re-enqueued {:?} peer job for resumed run {} (coordinator may be past the \
             config-bearing command — run will fail if so; remediation: dismiss and re-accept)",
            run.kind,
            run.instance_name
        );
    }
}

pub(crate) async fn mark_failed_via_pool(db: &SqlitePool, instance_name: &str, error: &str) {
    if let Err(e) = set_run_status(
        db,
        instance_name,
        WorkflowProgress::Failed,
        Some(error.to_string()),
    )
    .await
    {
        tracing::warn!("Failed to mark resumed run {instance_name} failed: {e:#}");
    }
}

async fn set_run_status(
    db: &SqlitePool,
    instance_name: &str,
    status: WorkflowProgress,
    error: Option<String>,
) -> Result {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tx = db.begin_transaction().await?;
    tx.set_workflow_run_status(instance_name, status, error.as_deref(), now)
        .await?;
    Commitable::commit(tx).await
}
