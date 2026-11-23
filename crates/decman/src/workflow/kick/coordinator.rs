use std::{collections::HashSet, sync::Arc};

use anyhow::Context;

use crate::{
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        signature_pairs::save_signature_pairs,
        storage::{WorkflowStorage, artifact_kinds},
        submit_topology,
    },
};

use super::{
    KickConfig, KickStep,
    steps::{create_proposals, export_state, prune_cached_membership, submit_kick},
};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    kick_config: KickConfig,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result {
    tracing::info!("Initializing Noise server...");

    // Exclude the participant being kicked from peers
    let excluded_participants = kick_config
        .participant_id
        .iter()
        .map(ToString::to_string)
        .collect();

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        kick_config.instance_name.clone(),
        KickStep::WaitingForPeers,
        Some(excluded_participants),
        last_seen,
    )
    .await?;

    let workflow_state = server.get_workflow_state();
    let server = Arc::new(server);

    let coordinator_workflow = {
        let workflow_state = workflow_state.clone();
        let node_config = node_config.clone();
        let kick_config = kick_config.clone();
        let db = db.clone();
        let instance_name = kick_config.instance_name.clone();

        tokio::spawn(async move {
            let mut coordinator_completed_steps = HashSet::new();

            loop {
                let current_step = workflow_state.current_step().await;
                tracing::debug!("Coordinator in step: {current_step:?}");

                match current_step {
                    KickStep::WaitingForPeers => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::ExportState => {
                        if !coordinator_completed_steps.contains(&KickStep::ExportState) {
                            tracing::info!("Coordinator executing: Export state");
                            export_state(&node_config, &db, &instance_name, &kick_config).await?;
                            coordinator_completed_steps.insert(KickStep::ExportState);
                            workflow_state.advance_step().await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::CreateProposals => {
                        tracing::info!("Coordinator executing: Create proposals");
                        create_proposals(&node_config, &db, &instance_name, &kick_config).await?;

                        // Load kick proposals to send to peers with SignKick command.
                        // Combine kick config + DNS and P2P kick proposals into a single
                        // length-prefixed payload (decoded as 3 items by the peer).
                        let dns_data = db
                            .read_artifact(&instance_name, artifact_kinds::KICK_DNS_PROPOSAL, None)
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "KICK_DNS_PROPOSAL artifact missing after CreateProposals"
                                )
                            })?;
                        let p2p_data = db
                            .read_artifact(&instance_name, artifact_kinds::KICK_P2P_PROPOSAL, None)
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "KICK_P2P_PROPOSAL artifact missing after CreateProposals"
                                )
                            })?;

                        let config_data = serde_json::to_vec(&kick_config)
                            .context("Failed to serialize kick config")?;

                        let payload =
                            utils::encode_length_prefixed(&[&config_data, &dns_data, &p2p_data]);
                        workflow_state.set_command_payload(payload).await;
                        workflow_state.advance_step().await;
                    }
                    KickStep::SignProposals => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    KickStep::SubmitKick => {
                        tracing::info!("Coordinator executing: Submit kick");

                        save_signature_pairs(
                            &db,
                            &instance_name,
                            artifact_kinds::KICK_SIGNATURE_UPLOAD,
                            artifact_kinds::SIGNED_KICK_DNS,
                            artifact_kinds::SIGNED_KICK_P2P,
                        )
                        .await?;

                        let (confirmed_version, confirmed_thresholds) = {
                            let (config, storage, name) =
                                (node_config.clone(), db.clone(), instance_name.clone());
                            submit_topology(&db, &instance_name, async move {
                                submit_kick(&config, &storage, &name).await
                            })
                            .await?
                        };
                        // Deliberately not fatal. The kick is durable at this
                        // point, so failing the step would leave the workflow
                        // on SubmitKick and a resume would resubmit the same
                        // topology transactions. A stale cache is recoverable
                        // by the next /decentralized-parties refresh; a
                        // rewound kick is not.
                        if let Err(e) = prune_cached_membership(
                            &db,
                            &kick_config.decentralized_party_id,
                            kick_config.participant_id.as_ref(),
                            &kick_config.namespace_fingerprint,
                            confirmed_version,
                            confirmed_thresholds,
                        )
                        .await
                        {
                            tracing::warn!(
                                "Kick submitted but pruning owner key {} ({:?}) from cached \
                                 membership failed: {e}. Refresh /decentralized-parties before \
                                 re-adding it.",
                                kick_config.namespace_fingerprint,
                                kick_config.participant_id
                            );
                        }
                        workflow_state.advance_step().await;
                    }
                    KickStep::RetireLegacyKeys => {
                        crate::workflow::topology::retire_legacy_keys_for_run(
                            &node_config,
                            &db,
                            &instance_name,
                            &kick_config.decentralized_party_id,
                        )
                        .await?;
                        workflow_state.advance_step().await;
                    }
                    KickStep::Complete => {
                        tracing::info!("Kick workflow complete!");
                        tracing::debug!("Waiting for peers to receive Disconnect command...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        break;
                    }
                }
            }

            Ok(())
        })
    };

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::Kick(server),
        instance,
        coordinator_workflow,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use sqlx::SqlitePool;
    use tokio::sync::RwLock;

    use super::*;
    use crate::{
        canton_id::{CantonId, NAMESPACE_LENGTH, Namespace},
        db::MIGRATOR,
        noise::NoiseKeypair,
        server::{WorkflowKind, WorkflowRole},
        signing::{custody::SoftwareKeys, fake_admin_api::FakeAdminApi},
        workflow::state::WorkflowStep,
    };

    const RUN: &str = "kick-run";

    /// Cancelling a run aborts the task that drives it. A kick that is at the
    /// legacy key retirement then must write no topology. Before the loop
    /// stopped with its driver, it went on to sign the removal.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_aborted_kick_run_signs_no_topology(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![]).await?;
        let party = CantonId::new("party".to_string(), Namespace::new([7; NAMESPACE_LENGTH]));
        fake.publish_party_awaiting_key_retirement(&party);
        let node = fake.node(SoftwareKeys::Allowed, false).await?;
        tokio::fs::create_dir_all(node.config.data_dir()).await?;
        NoiseKeypair::generate()
            .save_to_file(node.config.key_file_path())
            .await?;

        let kick_config = KickConfig::new(
            party,
            None,
            "fingerprint".to_string(),
            1,
            None,
            2,
            RUN.to_string(),
        );
        let step = KickStep::RetireLegacyKeys;
        sqlx::query(
            "INSERT INTO workflow_runs
             (instance_name, kind, role, status, current_step, step_index, step_total,
              config_json, expected_peers_json, completed_peers_json, created_at, updated_at)
             VALUES (?, 'Kick', 'Coordinator', 'inprogress', ?, ?, ?, ?, '[]', '[]', 0, 0)",
        )
        .bind(RUN)
        .bind(step.step_name())
        .bind(step.step_index())
        .bind(KickStep::step_total())
        .bind(serde_json::to_string(&kick_config)?)
        .execute(&db)
        .await?;

        let instance = WorkflowInstance::new(
            RUN.to_string(),
            WorkflowKind::Kick,
            WorkflowRole::Coordinator,
        );
        let driver = tokio::spawn(start_coordinator(
            node.config.clone(),
            NetworkConfig::default(),
            kick_config,
            db,
            Arc::new(RwLock::new(HashMap::new())),
            instance.clone(),
        ));
        // The driver registers the run's Noise handle right after it starts
        // the coordinator loop, which is the loop's first moment to run.
        while instance.active.read().is_ok_and(|active| active.is_none()) {
            tokio::task::yield_now().await;
        }

        driver.abort();
        assert!(driver.await.is_err_and(|e| e.is_cancelled()));
        tokio::time::sleep(Duration::from_secs(2)).await;

        assert!(
            fake.authorized().is_empty(),
            "the kick signed topology after its run was aborted"
        );
        Ok(())
    }
}
