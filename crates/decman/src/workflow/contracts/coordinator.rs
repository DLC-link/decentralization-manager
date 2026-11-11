use std::sync::Arc;

use anyhow::Context;
use tokio::time::Duration;

use super::{
    ContractsConfig, ContractsStep,
    steps::{execute_submissions, prepare_submissions, sign_submissions},
};
use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer, lookup_party_threshold},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        CoordinatorSteps, StepFlow, run_coordinator,
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Run the contracts workflow on this node as its coordinator, until the run
/// completes or fails.
///
/// # Errors
///
/// Returns an error if the Noise key file cannot be read, if no workflow auth
/// is configured, or if the party's credentials cannot be obtained. Returns an
/// error if a step fails, for example a Ledger API call or a read or write of
/// the run's stored artefacts. Returns an error if a check fails, for example
/// on the party's threshold, the prepared creates or this node's signing key.
/// Returns an error if the peers the run waits on give up or go silent, or if
/// the workflow task panics.
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
        network_config,
        db.clone(),
        config.instance_name.clone(),
        ContractsStep::WaitingForPeers,
        None, // No excluded participants
        last_seen,
    )
    .await?;

    tracing::info!("Noise server initialized, listening for connections");

    let steps = ContractsSteps::start(node_config, db, config, workflow_auth);
    run_coordinator(server, ActiveWorkflow::Contracts, instance, steps).await
}

/// The contracts coordinator's work at each step.
struct ContractsSteps {
    node_config: NodeConfig,
    db: sqlx::SqlitePool,
    config: ContractsConfig,
    instance_name: String,
    dec_party_id: CantonId,
    coordinator_only: bool,
    auth: WorkflowAuth,
}

impl ContractsSteps {
    /// Look up the party's threshold and take the run's auth handle.
    async fn start(
        node_config: NodeConfig,
        db: sqlx::SqlitePool,
        config: ContractsConfig,
        workflow_auth: Option<WorkflowAuth>,
    ) -> Result<Self> {
        let instance_name = config.instance_name.clone();
        let dec_party_id = config.decentralized_party_id.clone();

        // 1-of-N party: the coordinator's own signature already meets the threshold,
        // so no peer signatures are required. The peer gates (WaitingForPeers /
        // SignSubmissions) would otherwise wait for a peer that may never connect;
        // the coordinator advances them itself below. Any other threshold (or an
        // unresolved one) falls back to the normal peer wait.
        let coordinator_only = lookup_party_threshold(&db, &dec_party_id).await == Some(1);

        // Auth handle for the decentralized party. We deliberately fetch a fresh
        // token at each ledger-touching step rather than caching one snapshot up
        // front: a workflow can sit in `WaitingForPeers` for an arbitrarily long
        // time, and a token captured before that wait would be expired by the time
        // Prepare/Execute run automatically once peers accept. `get_credentials`
        // returns a cached-with-refresh token, so per-step calls are cheap.
        let auth = workflow_auth
            .ok_or_else(|| anyhow::anyhow!("Auth not configured, cannot run contracts workflow"))?;

        Ok(Self {
            node_config,
            db,
            config,
            instance_name,
            dec_party_id,
            coordinator_only,
            auth,
        })
    }
}

impl CoordinatorSteps<ContractsStep> for ContractsSteps {
    async fn run_step(
        &mut self,
        workflow_state: &WorkflowState<ContractsStep>,
        current_step: ContractsStep,
    ) -> Result<StepFlow> {
        match current_step {
            ContractsStep::WaitingForPeers => {
                if self.coordinator_only {
                    workflow_state
                        .advance_step_if(ContractsStep::WaitingForPeers)
                        .await;
                }
                Ok(StepFlow::Idle)
            }
            ContractsStep::PrepareSubmissions => {
                tracing::info!("Coordinator executing: Prepare submissions");
                let creds = self
                    .auth
                    .get_credentials(&self.config.decentralized_party_id)
                    .await?;
                prepare_submissions(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &self.config,
                    &creds.token,
                    &creds.user_id,
                )
                .await?;

                // Load prepared submissions from storage to ship to peers with the
                // SignSubmissions command. Pair with the contracts config so the
                // peer can recover the instance name + dec party id on its side.
                let submissions_payload =
                    load_prepared_submissions_payload(&self.db, &self.instance_name, &self.config)
                        .await?;
                workflow_state
                    .set_command_payload(submissions_payload)
                    .await;

                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            ContractsStep::SignSubmissions => {
                tracing::info!("Coordinator executing: Sign submissions");
                let intents = &self.config.committed()?.intents;
                sign_submissions(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &self.dec_party_id,
                    intents,
                )
                .await
                .context("Failed to sign submissions")?;
                if self.coordinator_only {
                    // Coordinator's signature (just produced) already meets M=1;
                    // advance without waiting for peer signatures.
                    workflow_state
                        .advance_step_if(ContractsStep::SignSubmissions)
                        .await;
                }
                Ok(StepFlow::Idle)
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
                    self.db
                        .write_artifact(
                            &self.instance_name,
                            artifact_kinds::SUBMISSION_SIGNATURES,
                            Some(&peer_key),
                            payload,
                        )
                        .await?;
                }
                workflow_state.clear_peer_data().await;

                let creds = self
                    .auth
                    .get_credentials(&self.config.decentralized_party_id)
                    .await?;
                execute_submissions(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &self.config,
                    &creds.token,
                    &creds.user_id,
                )
                .await?;
                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            ContractsStep::Complete => {
                tracing::info!("Contracts workflow complete!");
                Ok(StepFlow::Finish(Duration::from_secs(5)))
            }
        }
    }
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
