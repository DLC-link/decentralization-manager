use std::{collections::HashSet, sync::Arc};

use anyhow::Context;

use crate::{
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

use super::{
    RotateKeyConfig, RotateKeyStep,
    steps::{create_proposal, generate_key, submit_rotation},
};

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

    let workflow_state = server.get_workflow_state();
    let server = Arc::new(server);

    let coordinator_workflow = {
        let workflow_state = workflow_state.clone();
        let node_config = node_config.clone();
        let rotate_config = rotate_config.clone();
        let db = db.clone();
        let instance_name = rotate_config.instance_name.clone();

        tokio::spawn(async move {
            let mut completed = HashSet::new();

            loop {
                let current_step = workflow_state.current_step().await;
                tracing::debug!("Coordinator in step: {current_step:?}");

                match current_step {
                    RotateKeyStep::WaitingForPeers => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    // The payload is set before each of these steps starts. A
                    // resumed coordinator holds none, so it rebuilds it here.
                    RotateKeyStep::SignProposal | RotateKeyStep::RecordKey => {
                        if workflow_state.get_command_payload().await.is_empty() {
                            let payload = if current_step == RotateKeyStep::SignProposal {
                                sign_payload(&db, &instance_name, &rotate_config).await?
                            } else {
                                record_payload(&db, &instance_name).await?
                            };
                            workflow_state.set_command_payload(payload).await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    RotateKeyStep::GenerateKey => {
                        if completed.insert(RotateKeyStep::GenerateKey) {
                            tracing::info!("Coordinator executing: Generate key");
                            generate_key(&node_config, &db, &instance_name, &rotate_config).await?;
                            workflow_state.advance_step().await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    RotateKeyStep::CreateProposal => {
                        tracing::info!("Coordinator executing: Create proposal");
                        create_proposal(&node_config, &db, &instance_name, &rotate_config).await?;
                        workflow_state
                            .set_command_payload(
                                sign_payload(&db, &instance_name, &rotate_config).await?,
                            )
                            .await;
                        workflow_state.advance_step().await;
                    }
                    RotateKeyStep::Submit => {
                        tracing::info!("Coordinator executing: Submit rotate-key");
                        for (peer_id, signed) in workflow_state.get_all_peer_data().await {
                            db.write_artifact(
                                &instance_name,
                                artifact_kinds::SIGNED_ROTATE_KEY_P2P,
                                Some(&peer_id.to_string()),
                                &signed,
                            )
                            .await?;
                        }
                        workflow_state.clear_peer_data().await;

                        submit_rotation(&node_config, &db, &instance_name, &rotate_config).await?;
                        workflow_state.advance_step().await;
                    }
                    RotateKeyStep::RetireLegacyKeys => {
                        crate::workflow::topology::retire_legacy_keys_and_wait(
                            &node_config,
                            &rotate_config.decentralized_party_id,
                        )
                        .await?;
                        workflow_state
                            .set_command_payload(record_payload(&db, &instance_name).await?)
                            .await;
                        workflow_state.advance_step().await;
                    }
                    RotateKeyStep::Complete => {
                        tracing::info!("Rotate-key workflow complete!");
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        break;
                    }
                }
            }

            Ok(())
        })
    };

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::RotateKey(server),
        instance,
        coordinator_workflow,
    )
    .await
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
