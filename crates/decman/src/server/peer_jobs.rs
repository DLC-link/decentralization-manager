//! The peer-side job listener that starts a peer run per accepted invite.

use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{RwLock, mpsc};

use crate::{
    auth::WorkflowAuth,
    config::NodeConfig,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    server::{PeerJob, types::WorkflowProgress},
    utils::now_secs,
    workflow,
};

/// Take the peer-side `instance_name` slot and flip the matching
/// `workflow_runs` row to a terminal status. No-op if no slot was set (the
/// peer row creation may have failed; we don't want to over-write
/// unrelated state).
/// Mark a finished peer-side run terminal in the DB (the notification feed is
/// driven off `workflow_runs`, so peer status flows through here rather than an
/// in-memory singleton). `instance_name` is the peer-side row's primary key.
async fn finalize_peer_run(
    db: &SqlitePool,
    instance_name: &str,
    success: bool,
    error_msg: Option<String>,
) {
    let now = now_secs();
    let mut tx = match db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("finalize_peer_run: begin_transaction failed: {e}");
            return;
        }
    };
    let status = if success {
        WorkflowProgress::Completed
    } else {
        WorkflowProgress::Failed
    };
    if let Err(e) = tx
        .set_workflow_run_status(instance_name, status, error_msg.as_deref(), now)
        .await
    {
        tracing::warn!("finalize_peer_run: update failed: {e}");
        return;
    }
    if let Err(e) = Commitable::commit(tx).await {
        tracing::warn!("finalize_peer_run: commit failed: {e}");
    }
}

/// Single peer-job listener. Drains the `PeerJob` queue and spawns one
/// `workflow::start_peer` task per job, so a node can participate as a peer in
/// any number of concurrent workflows at once (the cross-acceptance scenario).
/// Each job carries its own peer-side `instance_name`, the coordinator's run
/// `instance_name` (for command routing), and the coordinator pubkey, so jobs
/// never race over a shared slot. Replaces the four per-kind trigger loops.
pub(super) async fn run_peer_listener(
    config: NodeConfig,
    db: SqlitePool,
    auth: Arc<RwLock<Option<WorkflowAuth>>>,
    mut peer_jobs: mpsc::UnboundedReceiver<PeerJob>,
) {
    while let Some(job) = peer_jobs.recv().await {
        let config = config.clone();
        let db = db.clone();
        let auth = auth.clone();
        tokio::spawn(async move {
            tracing::info!(
                "Starting {:?} peer workflow {} (coordinator run {:?})",
                job.kind,
                job.instance_name,
                job.coordinator_instance
            );

            // Resolve the coordinator peer from its stored public key.
            let coordinator = match db.get_peer_by_public_key(&job.coordinator_pubkey).await {
                Ok(Some(peer)) => peer,
                Ok(None) => {
                    tracing::error!(
                        "Coordinator with pubkey {} not found; failing peer run {}",
                        job.coordinator_pubkey,
                        job.instance_name
                    );
                    finalize_peer_run(
                        &db,
                        &job.instance_name,
                        false,
                        Some("Coordinator not found in peer database".to_string()),
                    )
                    .await;
                    return;
                }
                Err(e) => {
                    tracing::error!("Failed to look up coordinator peer: {e}");
                    finalize_peer_run(&db, &job.instance_name, false, Some(format!("{e}"))).await;
                    return;
                }
            };

            let auth_snapshot = auth.read().await.clone();
            let result = workflow::start_peer(
                config,
                coordinator,
                db.clone(),
                job.instance_name.clone(),
                job.coordinator_instance.clone(),
                auth_snapshot,
            )
            .await;

            let success = result.is_ok();
            let err_msg = result.as_ref().err().map(|e| format!("{e}"));
            match &result {
                Ok(()) => tracing::info!(
                    "{:?} peer workflow {} completed",
                    job.kind,
                    job.instance_name
                ),
                Err(e) => tracing::error!(
                    "{:?} peer workflow {} failed: {e}",
                    job.kind,
                    job.instance_name
                ),
            }
            finalize_peer_run(&db, &job.instance_name, success, err_msg).await;
        });
    }
    tracing::warn!("peer-job listener exited: all senders dropped");
}
