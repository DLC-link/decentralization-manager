use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use tokio::time::Duration;

use super::{
    RotateKeyConfig, RotateKeyStep,
    steps::{create_proposal, generate_key, submit_rotation},
};
use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        CoordinatorSteps, StepFlow, run_coordinator,
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds},
        topology::retire_legacy_keys_and_wait,
    },
};

/// Run the rotate-key workflow on this node as its coordinator, until the run
/// completes or fails.
///
/// # Errors
///
/// Returns an error if the Noise key file cannot be read. Returns an error if a
/// step fails, for example a Canton Admin API call, a vault or key store call,
/// or a read or write of the run's stored artefacts. Returns an error if the
/// saved proposal or new key is missing, or if a peer's signature is not on
/// record at submit. Returns an error if the peers the run waits on give up or
/// go silent, or if the workflow task panics.
pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    rotate_config: RotateKeyConfig,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result {
    tracing::info!("Initializing Noise server...");

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        rotate_config.instance_name.clone(),
        RotateKeyStep::WaitingForPeers,
        None,
        last_seen,
    )
    .await?;

    let steps = RotateKeySteps {
        instance_name: rotate_config.instance_name.clone(),
        node_config,
        rotate_config,
        db,
        completed: HashSet::new(),
    };
    run_coordinator(server, ActiveWorkflow::RotateKey, instance, async {
        Ok(steps)
    })
    .await
}

/// The rotate-key coordinator's work at each step.
struct RotateKeySteps {
    node_config: NodeConfig,
    rotate_config: RotateKeyConfig,
    db: sqlx::SqlitePool,
    instance_name: String,
    /// The steps this node has run that it must not run again.
    completed: HashSet<RotateKeyStep>,
}

impl CoordinatorSteps<RotateKeyStep> for RotateKeySteps {
    async fn run_step(
        &mut self,
        workflow_state: &WorkflowState<RotateKeyStep>,
        current_step: RotateKeyStep,
    ) -> Result<StepFlow> {
        tracing::debug!("Coordinator in step: {current_step:?}");

        match current_step {
            RotateKeyStep::WaitingForPeers => Ok(StepFlow::Idle),
            // The payload is set before each of these steps starts. A
            // resumed coordinator holds none, so it rebuilds it here.
            RotateKeyStep::SignProposal | RotateKeyStep::RecordKey => {
                if workflow_state.get_command_payload().await.is_empty() {
                    let payload = if current_step == RotateKeyStep::SignProposal {
                        sign_payload(&self.db, &self.instance_name, &self.rotate_config).await?
                    } else {
                        record_payload(&self.db, &self.instance_name).await?
                    };
                    workflow_state.set_command_payload(payload).await;
                }
                Ok(StepFlow::Idle)
            }
            RotateKeyStep::GenerateKey => {
                if self.completed.insert(RotateKeyStep::GenerateKey) {
                    tracing::info!("Coordinator executing: Generate key");
                    generate_key(
                        &self.node_config,
                        &self.db,
                        &self.instance_name,
                        &self.rotate_config,
                    )
                    .await?;
                    workflow_state.advance_step().await;
                }
                Ok(StepFlow::Idle)
            }
            RotateKeyStep::CreateProposal => {
                tracing::info!("Coordinator executing: Create proposal");
                create_proposal(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &self.rotate_config,
                )
                .await?;
                workflow_state
                    .set_command_payload(
                        sign_payload(&self.db, &self.instance_name, &self.rotate_config).await?,
                    )
                    .await;
                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            RotateKeyStep::Submit => {
                tracing::info!("Coordinator executing: Submit rotate-key");
                require_every_signature(
                    &self.db,
                    &self.instance_name,
                    workflow_state.expected_peers(),
                )
                .await?;
                submit_rotation(
                    &self.node_config,
                    &self.db,
                    &self.instance_name,
                    &self.rotate_config,
                )
                .await?;
                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            RotateKeyStep::RetireLegacyKeys => {
                retire_legacy_keys_and_wait(
                    &self.node_config,
                    &self.rotate_config.decentralized_party_id,
                )
                .await?;
                workflow_state
                    .set_command_payload(record_payload(&self.db, &self.instance_name).await?)
                    .await;
                workflow_state.advance_step().await;
                Ok(StepFlow::Continue)
            }
            RotateKeyStep::Complete => {
                tracing::info!("Rotate-key workflow complete!");
                Ok(StepFlow::Finish(Duration::from_secs(2)))
            }
        }
    }
}

/// Fail unless every peer's signature is on record. The Noise server stores
/// each upload before it counts the peer complete, so a gap means the run lost
/// state some other way. Canton would refuse the submission without it.
async fn require_every_signature(
    db: &sqlx::SqlitePool,
    instance_name: &str,
    peers: &HashSet<CantonId>,
) -> Result {
    let recorded: HashSet<String> = db
        .list_artifacts(instance_name, artifact_kinds::SIGNED_ROTATE_KEY_P2P)
        .await?
        .into_iter()
        .map(|(peer, _)| peer)
        .collect();
    let mut missing: Vec<String> = peers
        .iter()
        .map(ToString::to_string)
        .filter(|peer| !recorded.contains(peer))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    missing.sort();
    anyhow::bail!(
        "No rotate-key signature on record from {missing}. Start a new rotation.",
        missing = missing.join(", ")
    )
}

/// The `SignRotateKey` payload: the run's config and the coordinator's signed
/// proposal.
async fn sign_payload(
    db: &sqlx::SqlitePool,
    instance_name: &str,
    rotate_config: &RotateKeyConfig,
) -> Result<Vec<u8>> {
    let proposal = db
        .read_artifact(instance_name, artifact_kinds::ROTATE_KEY_P2P_PROPOSAL, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("ROTATE_KEY_P2P_PROPOSAL artifact missing"))?;
    let config_data =
        serde_json::to_vec(rotate_config).context("Failed to serialize rotate-key config")?;
    Ok(utils::encode_length_prefixed(&[&config_data, &proposal]))
}

/// The `RecordRotatedKey` payload: the new key.
async fn record_payload(db: &sqlx::SqlitePool, instance_name: &str) -> Result<Vec<u8>> {
    db.read_artifact(instance_name, artifact_kinds::ROTATE_KEY_NEW_KEY, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("ROTATE_KEY_NEW_KEY artifact missing"))
}

#[cfg(test)]
mod tests {
    use common::types::{WorkflowKind, WorkflowRole};

    use super::*;
    use crate::canton_id::{NAMESPACE_LENGTH, Namespace};

    const RUN: &str = "rotate-key-run";

    fn peer(n: u8) -> CantonId {
        CantonId::new(format!("p{n}"), Namespace::new([n; NAMESPACE_LENGTH]))
    }

    async fn insert_run(db: &sqlx::SqlitePool) -> Result {
        sqlx::query(
            "INSERT INTO workflow_runs
             (instance_name, kind, role, status, current_step, step_index, step_total,
              config_json, expected_peers_json, completed_peers_json, created_at, updated_at)
             VALUES (?, ?, ?, 'inprogress', 'Submit', 4, 8, '{}', '[]', '[]', 0, 0)",
        )
        .bind(RUN)
        .bind(WorkflowKind::RotateKey.as_str())
        .bind(WorkflowRole::Coordinator.as_str())
        .execute(db)
        .await?;
        Ok(())
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn submit_needs_a_signature_from_every_peer(db: sqlx::SqlitePool) -> Result {
        insert_run(&db).await?;
        db.write_artifact(
            RUN,
            artifact_kinds::SIGNED_ROTATE_KEY_P2P,
            Some(&peer(1).to_string()),
            b"signed by p1",
        )
        .await?;
        let peers: HashSet<CantonId> = [peer(1), peer(2)].into();

        let error = require_every_signature(&db, RUN, &peers)
            .await
            .expect_err("a run without p2's signature must not submit")
            .to_string();
        assert!(error.contains(&peer(2).to_string()), "{error}");
        assert!(!error.contains(&peer(1).to_string()), "{error}");

        db.write_artifact(
            RUN,
            artifact_kinds::SIGNED_ROTATE_KEY_P2P,
            Some(&peer(2).to_string()),
            b"signed by p2",
        )
        .await?;
        require_every_signature(&db, RUN, &peers).await
    }
}
