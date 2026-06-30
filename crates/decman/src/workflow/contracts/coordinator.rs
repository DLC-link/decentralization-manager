use std::sync::Arc;

use anyhow::Context;

use crate::{
    auth::WorkflowAuth,
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

use super::{
    ContractsConfig, ContractsStep,
    steps::{execute_submissions, prepare_submissions, sign_submissions},
};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    config: ContractsConfig,
    workflow_auth: Option<WorkflowAuth>,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result {
    tracing::info!("Initializing Noise server...");

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        config.instance_name.clone(),
        ContractsStep::WaitingForPeers,
        None, // No excluded participants
        last_seen,
    )
    .await?;
    let server = Arc::new(server);

    tracing::info!("Noise server initialized, listening for connections");

    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let db_clone = db.clone();
    let workflow_handle = tokio::spawn(async move {
        run_workflow(
            workflow_state,
            node_config_clone,
            db_clone,
            config,
            workflow_auth,
        )
        .await
    });

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::Contracts(server),
        instance,
        workflow_handle,
    )
    .await
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<ContractsStep>>,
    node_config: NodeConfig,
    db: sqlx::SqlitePool,
    config: ContractsConfig,
    workflow_auth: Option<WorkflowAuth>,
) -> Result {
    let instance_name = config.instance_name.clone();
    let dec_party_id = config.decentralized_party_id.clone();

    // Auth handle for the decentralized party. We deliberately fetch a fresh
    // token at each ledger-touching step rather than caching one snapshot up
    // front: a workflow can sit in `WaitingForPeers` for an arbitrarily long
    // time, and a token captured before that wait would be expired by the time
    // Prepare/Execute run automatically once peers accept. `get_credentials`
    // returns a cached-with-refresh token, so per-step calls are cheap.
    let auth = workflow_auth
        .ok_or_else(|| anyhow::anyhow!("Auth not configured, cannot run contracts workflow"))?;

    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            ContractsStep::WaitingForPeers => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::PrepareSubmissions => {
                tracing::info!("Coordinator executing: Prepare submissions");
                let creds = auth.get_credentials(&config.decentralized_party_id).await?;
                prepare_submissions(
                    &node_config,
                    &db,
                    &instance_name,
                    &config,
                    &creds.token,
                    &creds.user_id,
                )
                .await?;

                // Load prepared submissions from storage to ship to peers with the
                // SignSubmissions command. Pair with the contracts config so the
                // peer can recover the instance name + dec party id on its side.
                let submissions_payload =
                    load_prepared_submissions_payload(&db, &instance_name, &config).await?;
                workflow_state
                    .set_command_payload(submissions_payload)
                    .await;

                workflow_state.advance_step().await;
            }
            ContractsStep::SignSubmissions => {
                tracing::info!("Coordinator executing: Sign submissions");
                sign_submissions(&node_config, &db, &instance_name, &dec_party_id)
                    .await
                    .context("Failed to sign submissions")?;
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::ExecuteSubmissions => {
                tracing::info!("Coordinator executing: Execute submissions");

                // Persist each peer's `SUBMISSION_SIGNATURES` blob into
                // workflow storage keyed by peer id, so `execute_submissions`
                // can read them via `list_artifacts`. The coordinator's own
                // bundle was already written by `sign_submissions` above.
                let peer_data = workflow_state.get_all_peer_data().await;
                for (peer_id, payload) in &peer_data {
                    let peer_key = peer_id.to_string();
                    db.write_artifact(
                        &instance_name,
                        artifact_kinds::SUBMISSION_SIGNATURES,
                        Some(&peer_key),
                        payload,
                    )
                    .await?;
                }
                workflow_state.clear_peer_data().await;

                let creds = auth.get_credentials(&config.decentralized_party_id).await?;
                execute_submissions(
                    &node_config,
                    &db,
                    &instance_name,
                    &config,
                    &creds.token,
                    &creds.user_id,
                )
                .await?;
                workflow_state.advance_step().await;
            }
            ContractsStep::Complete => {
                tracing::info!("Contracts workflow complete!");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}

/// Load all prepared submission artefacts from storage and encode them for
/// transmission to peers. The blobs are keyed by zero-padded ordinal so
/// `list_artifacts` returns them in their original creation order, which the
/// receiving peer preserves when it re-keys them on its side.
///
/// Returns a payload of shape `[config_json][files_payload]` (length-prefixed),
/// where each file is `(ordinal, blob)` — same wire shape the previous
/// file-based implementation produced.
async fn load_prepared_submissions_payload(
    db: &sqlx::SqlitePool,
    instance_name: &str,
    config: &ContractsConfig,
) -> Result<Vec<u8>> {
    tracing::info!("Loading prepared submissions from storage for distribution");

    let submission_rows = db
        .list_artifacts(instance_name, artifact_kinds::PREPARED_SUBMISSION)
        .await?;

    if submission_rows.is_empty() {
        anyhow::bail!("No PREPARED_SUBMISSION artifacts found for instance {instance_name}");
    }

    tracing::info!(
        "Loaded {count} prepared submission artefact(s) for distribution to peers",
        count = submission_rows.len()
    );

    let config_data = serde_json::to_vec(config).context("Failed to serialize contracts config")?;
    let files_payload = utils::encode_files(&submission_rows);
    Ok(utils::encode_length_prefixed(&[
        &config_data,
        &files_payload,
    ]))
}
