use std::{sync::Arc, time::SystemTime};

use anyhow::Context;
use sqlx::SqlitePool;

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::DARS_PEER_JOIN_LIMIT,
    db::schema::SchemaRead,
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{contracts, state::WorkflowState},
};

use super::{DarsConfig, DarsStep, load_bundle};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    config: DarsConfig,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result {
    tracing::info!("Initializing Noise server for DARs upload...");

    let self_id = node_config.participant_id();
    let excluded: Vec<String> = network_config
        .peers
        .iter()
        .filter(|p| &p.participant_id != self_id && !config.peer_ids.contains(&p.participant_id))
        .map(|p| p.participant_id.to_string())
        .collect();

    let instance_name = config.instance_name;
    let server = NoiseServer::new(
        node_config.clone(),
        network_config,
        db.clone(),
        instance_name.clone(),
        DarsStep::WaitingForPeers,
        Some(excluded),
        last_seen,
    )
    .await?;
    let server = Arc::new(server);

    let workflow_state = server.get_workflow_state();
    // A run resumed mid-transfer must serve its DARs on the first poll. The run
    // takes no peer requests until `run_workflow_with_handler`, so loading them
    // here means no peer is ever sent an empty `UploadDars`.
    if workflow_state.current_step().await == DarsStep::UploadDars {
        workflow_state
            .set_command_payload(load_bundle(&db, &instance_name).await?)
            .await;
    }
    let waiting_since = waiting_since(&db, &instance_name).await?;

    tracing::info!("Noise server initialized, listening for connections");

    let workflow_handle = tokio::spawn(async move {
        run_workflow(
            workflow_state,
            node_config,
            db,
            instance_name,
            waiting_since,
        )
        .await
    });

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::Dars(server),
        instance,
        workflow_handle,
    )
    .await
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<DarsStep>>,
    node_config: NodeConfig,
    db: SqlitePool,
    instance_name: String,
    waiting_since: i64,
) -> Result {
    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            DarsStep::WaitingForPeers => {
                check_join_deadline(&workflow_state, waiting_since, unix_now()).await?;
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            DarsStep::LoadDars => {
                workflow_state
                    .set_command_payload(load_bundle(&db, &instance_name).await?)
                    .await;
                workflow_state.advance_step_if(DarsStep::LoadDars).await;
            }
            DarsStep::UploadDars => {
                // While peers are downloading, this node ONLY serves chunks
                // (handled by the Noise listener). The coordinator's own DAR
                // upload is deferred to `Complete` so its CPU/IO doesn't
                // contend with — and stall — the chunk transfer to peers on
                // CPU-limited nodes.
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            DarsStep::Complete => {
                // All peers have finished downloading (the step only advances
                // here once every peer completed UploadDars), so there's no
                // chunk-serving load left to compete with. Upload our own DARs
                // now. (Reached once, then we break — re-runs only on a resume,
                // where re-uploading is idempotent.)
                workflow_state.clear_command_payload().await;
                tracing::info!("Coordinator executing: Upload DARs");
                let bundle = load_bundle(&db, &instance_name).await?;
                let dar_files = if bundle.is_empty() {
                    Vec::new()
                } else {
                    utils::decode_files(&bundle)?
                };
                contracts::upload_dars_from_bytes(&node_config, dar_files).await?;
                tracing::info!("DARs upload workflow complete!");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}

/// When the run began to wait for its peers, in Unix seconds.
///
/// The row's `updated_at` moves only on a step or status change, so for a run
/// still at `WaitingForPeers` it is when the run started or was last retried.
/// A restart leaves it alone, so restarts do not extend the wait.
async fn waiting_since(db: &SqlitePool, instance_name: &str) -> Result<i64> {
    let run = db
        .get_workflow_run(instance_name)
        .await?
        .with_context(|| format!("run {instance_name} has no row"))?;
    Ok(run.updated_at)
}

/// Fail the run once its peers have had `DARS_PEER_JOIN_LIMIT` to join.
async fn check_join_deadline(
    workflow_state: &WorkflowState<DarsStep>,
    waiting_since: i64,
    now: i64,
) -> Result {
    let limit = DARS_PEER_JOIN_LIMIT.as_secs();
    if now.saturating_sub(waiting_since) < i64::try_from(limit).unwrap_or(i64::MAX) {
        return Ok(());
    }
    let connected = workflow_state.connected_peers().await;
    let mut missing: Vec<String> = workflow_state
        .expected_peers()
        .iter()
        .filter(|peer| !connected.contains(*peer))
        .map(ToString::to_string)
        .collect();
    // Every peer is in, and the next poll opens the gate.
    if missing.is_empty() {
        return Ok(());
    }
    missing.sort();
    anyhow::bail!(
        "These peers did not join within {hours} hours: {missing}. Retry the run after \
         they accept the invitation.",
        hours = limit / 3600,
        missing = missing.join(", "),
    )
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
