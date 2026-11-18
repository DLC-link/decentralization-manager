use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use sqlx::SqlitePool;

use crate::{
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        signature_pairs::save_signature_pairs,
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

use super::{
    ChangeThresholdConfig, ChangeThresholdStep,
    steps::{create_proposals, export_state, submit_change},
};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    change_config: ChangeThresholdConfig,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result {
    tracing::info!("Initializing Noise server...");

    // Every remaining party member signs — no exclusions.
    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        change_config.instance_name.clone(),
        ChangeThresholdStep::WaitingForPeers,
        None,
        last_seen,
    )
    .await?;

    let workflow_state = server.get_workflow_state();
    let server = Arc::new(server);

    let coordinator_workflow = {
        let workflow_state = workflow_state.clone();
        let node_config = node_config.clone();
        let change_config = change_config.clone();
        let db = db.clone();
        let instance_name = change_config.instance_name.clone();

        tokio::spawn(async move {
            let mut coordinator_completed_steps = HashSet::new();

            loop {
                let current_step = workflow_state.current_step().await;
                tracing::debug!("Coordinator in step: {current_step:?}");

                match current_step {
                    ChangeThresholdStep::WaitingForPeers => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    ChangeThresholdStep::ExportState => {
                        if !coordinator_completed_steps.contains(&ChangeThresholdStep::ExportState)
                        {
                            tracing::info!("Coordinator executing: Export state");
                            export_state(&node_config, &db, &instance_name, &change_config).await?;
                            coordinator_completed_steps.insert(ChangeThresholdStep::ExportState);
                            workflow_state.advance_step().await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    ChangeThresholdStep::CreateProposals => {
                        tracing::info!("Coordinator executing: Create proposals");
                        create_proposals(&node_config, &db, &instance_name, &change_config).await?;

                        // Load proposals to send to peers with the
                        // SignChangeThreshold command. Combine config + DNS +
                        // P2P into a single length-prefixed payload (decoded as
                        // 3 items by the peer).
                        let dns_data = db
                            .read_artifact(
                                &instance_name,
                                artifact_kinds::CHANGE_THRESHOLD_DNS_PROPOSAL,
                                None,
                            )
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "CHANGE_THRESHOLD_DNS_PROPOSAL artifact missing after CreateProposals"
                                )
                            })?;
                        let p2p_data = db
                            .read_artifact(
                                &instance_name,
                                artifact_kinds::CHANGE_THRESHOLD_P2P_PROPOSAL,
                                None,
                            )
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "CHANGE_THRESHOLD_P2P_PROPOSAL artifact missing after CreateProposals"
                                )
                            })?;

                        let config_data = serde_json::to_vec(&change_config)
                            .context("Failed to serialize change-threshold config")?;

                        let payload =
                            utils::encode_length_prefixed(&[&config_data, &dns_data, &p2p_data]);
                        workflow_state.set_command_payload(payload).await;
                        workflow_state.advance_step().await;
                    }
                    ChangeThresholdStep::SignProposals => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    ChangeThresholdStep::Submit => {
                        tracing::info!("Coordinator executing: Submit change-threshold");

                        prepare_signatures(&workflow_state, &db, &instance_name).await?;

                        submit_change(&node_config, &db, &instance_name, &change_config).await?;
                        workflow_state.advance_step().await;
                    }
                    ChangeThresholdStep::RetireLegacyKeys => {
                        crate::workflow::topology::retire_legacy_keys_and_wait(
                            &node_config,
                            &change_config.decentralized_party_id,
                        )
                        .await?;
                        workflow_state.advance_step().await;
                    }
                    ChangeThresholdStep::Complete => {
                        tracing::info!("Change-threshold workflow complete!");
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
        ActiveWorkflow::ChangeThreshold(server),
        instance,
        coordinator_workflow,
    )
    .await
}

/// Rebuild the per-peer signature artifacts, then refuse to submit unless every
/// expected peer has both a DNS and a P2P signature.
///
/// Every remaining member signs a change-threshold run. The check uses the
/// expected set because advancing the signing step clears the completed set.
async fn prepare_signatures(
    workflow_state: &WorkflowState<ChangeThresholdStep>,
    storage: &SqlitePool,
    instance_name: &str,
) -> Result {
    save_signature_pairs(
        storage,
        instance_name,
        artifact_kinds::CHANGE_THRESHOLD_SIGNATURE_UPLOAD,
        artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
        artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
    )
    .await?;

    let missing_dns = peers_without_artifact(
        workflow_state,
        storage,
        instance_name,
        artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
    )
    .await?;
    let missing_p2p = peers_without_artifact(
        workflow_state,
        storage,
        instance_name,
        artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
    )
    .await?;
    if !missing_dns.is_empty() || !missing_p2p.is_empty() {
        anyhow::bail!(
            "Change-threshold cannot submit. Peers without a signed DNS proposal: \
             {missing_dns:?}. Peers without a signed P2P proposal: {missing_p2p:?}. \
             A coordinator restart under an older version can lose peer signatures, \
             and the peers do not send them again. Start a fresh change-threshold run."
        );
    }
    Ok(())
}

/// The expected peers that have no artifact of `artifact_kind`, sorted.
async fn peers_without_artifact(
    workflow_state: &WorkflowState<ChangeThresholdStep>,
    storage: &SqlitePool,
    instance_name: &str,
    artifact_kind: &str,
) -> Result<Vec<String>> {
    let stored: HashSet<String> = storage
        .list_artifacts(instance_name, artifact_kind)
        .await?
        .into_iter()
        .map(|(peer, _)| peer)
        .collect();
    let mut missing: Vec<String> = workflow_state
        .expected_peers()
        .iter()
        .map(ToString::to_string)
        .filter(|peer| !stored.contains(peer))
        .collect();
    missing.sort();
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        canton_id::{CantonId, NAMESPACE_LENGTH, Namespace},
        db::MIGRATOR,
    };

    fn peer(n: u8) -> CantonId {
        CantonId::new(format!("p{n}"), Namespace::new([n; NAMESPACE_LENGTH]))
    }

    async fn insert_run(pool: &SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO workflow_runs (
                 instance_name, kind, role, status, current_step, step_index, step_total,
                 config_json, expected_peers_json, completed_peers_json,
                 created_at, updated_at
             ) VALUES ('run-1', 'ChangeThreshold', 'Coordinator', 'inprogress', 'Submit',
                       4, 7, '{}', '[]', '[]', 0, 0)",
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// A coordinator resumed at `Submit`: advancing cleared the completed set.
    fn resumed_state(
        pool: &SqlitePool,
        expected: Vec<CantonId>,
    ) -> Arc<WorkflowState<ChangeThresholdStep>> {
        WorkflowState::from_persisted(
            pool.clone(),
            "run-1".into(),
            ChangeThresholdStep::Submit,
            expected,
            vec![],
            None,
        )
    }

    async fn write(
        pool: &SqlitePool,
        kind: &str,
        peer: &CantonId,
        data: &[u8],
    ) -> anyhow::Result<()> {
        pool.write_artifact("run-1", kind, Some(&peer.to_string()), data)
            .await?;
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn missing_artifacts_stop_the_submit_and_name_every_peer(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = resumed_state(&pool, vec![peer(1), peer(2)]);

        let message = prepare_signatures(&state, &pool, "run-1")
            .await
            .expect_err("a run with no peer signatures must not submit")
            .to_string();

        for peer in [peer(1), peer(2)] {
            let peer = peer.to_string();
            assert_eq!(message.matches(&peer).count(), 2, "{message}");
        }
        assert!(message.contains("fresh change-threshold run"), "{message}");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_dns_without_its_p2p_stops_the_submit(pool: SqlitePool) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = resumed_state(&pool, vec![peer(1), peer(2)]);
        for p in [peer(1), peer(2)] {
            write(
                &pool,
                artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
                &p,
                b"dns",
            )
            .await?;
        }
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
            &peer(1),
            b"p2p",
        )
        .await?;

        let message = prepare_signatures(&state, &pool, "run-1")
            .await
            .expect_err("a partial pair must not submit")
            .to_string();

        assert!(
            message.contains("without a signed DNS proposal: []"),
            "{message}"
        );
        assert!(
            message.contains(&format!("without a signed P2P proposal: [\"{}\"]", peer(2))),
            "{message}"
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_p2p_without_its_dns_stops_the_submit(pool: SqlitePool) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = resumed_state(&pool, vec![peer(1)]);
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
            &peer(1),
            b"p2p",
        )
        .await?;

        let message = prepare_signatures(&state, &pool, "run-1")
            .await
            .expect_err("a partial pair must not submit")
            .to_string();

        assert!(
            message.contains(&format!("without a signed DNS proposal: [\"{}\"]", peer(1))),
            "{message}"
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_missing_peer_stops_a_run_that_has_other_pairs(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = resumed_state(&pool, vec![peer(1), peer(2)]);
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            &peer(1),
            b"dns",
        )
        .await?;
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
            &peer(1),
            b"p2p",
        )
        .await?;

        let message = prepare_signatures(&state, &pool, "run-1")
            .await
            .expect_err("a peer with no signatures must not submit")
            .to_string();

        assert!(message.contains(&peer(2).to_string()), "{message}");
        assert!(!message.contains(&peer(1).to_string()), "{message}");
        Ok(())
    }

    /// An older binary already wrote the split artifacts and kept no upload.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn complete_legacy_split_artifacts_pass(pool: SqlitePool) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = resumed_state(&pool, vec![peer(1), peer(2)]);
        for p in [peer(1), peer(2)] {
            write(
                &pool,
                artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
                &p,
                b"dns",
            )
            .await?;
            write(
                &pool,
                artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
                &p,
                b"p2p",
            )
            .await?;
        }

        prepare_signatures(&state, &pool, "run-1").await
    }

    /// The restart can land between the two output writes of the split.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn durable_combined_uploads_rebuild_both_artifacts_after_a_restart(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = resumed_state(&pool, vec![peer(1), peer(2)]);
        // varint(len)||bytes twice: the DNS signature, then the P2P signature.
        for (p, combined) in [(peer(1), [1, 11, 1, 22]), (peer(2), [1, 33, 1, 44])] {
            write(
                &pool,
                artifact_kinds::CHANGE_THRESHOLD_SIGNATURE_UPLOAD,
                &p,
                &combined,
            )
            .await?;
        }
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            &peer(1),
            &[1, 11],
        )
        .await?;

        prepare_signatures(&state, &pool, "run-1").await?;

        for (p, dns, p2p) in [(peer(1), [1, 11], [1, 22]), (peer(2), [1, 33], [1, 44])] {
            let key = p.to_string();
            let read = |kind| pool.read_artifact("run-1", kind, Some(&key));
            assert_eq!(
                read(artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS).await?,
                Some(dns.to_vec())
            );
            assert_eq!(
                read(artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P).await?,
                Some(p2p.to_vec())
            );
        }
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn zero_expected_peers_pass_without_artifacts(pool: SqlitePool) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = resumed_state(&pool, vec![]);

        prepare_signatures(&state, &pool, "run-1").await
    }

    /// A completed peer is no proof of a stored signature.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn completed_peers_without_artifacts_stop_the_submit(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = WorkflowState::from_persisted(
            pool.clone(),
            "run-1".into(),
            ChangeThresholdStep::Submit,
            vec![peer(1), peer(2)],
            vec![peer(1), peer(2)],
            None,
        );

        let message = prepare_signatures(&state, &pool, "run-1")
            .await
            .expect_err("completed peers without signatures must not submit")
            .to_string();

        assert!(message.contains(&peer(1).to_string()), "{message}");
        assert!(message.contains(&peer(2).to_string()), "{message}");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_non_expected_peer_pair_does_not_cover_a_missing_peer(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = resumed_state(&pool, vec![peer(1)]);
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            &peer(3),
            b"dns",
        )
        .await?;
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
            &peer(3),
            b"p2p",
        )
        .await?;

        let message = prepare_signatures(&state, &pool, "run-1")
            .await
            .expect_err("a stranger's pair must not stand in for an expected peer")
            .to_string();

        let expected = format!("[\"{}\"]", peer(1));
        assert!(
            message.contains(&format!("without a signed DNS proposal: {expected}")),
            "{message}"
        );
        assert!(
            message.contains(&format!("without a signed P2P proposal: {expected}")),
            "{message}"
        );
        assert!(!message.contains(&peer(3).to_string()), "{message}");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_pair_from_another_run_does_not_count(pool: SqlitePool) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        sqlx::query(
            "INSERT INTO workflow_runs (
                 instance_name, kind, role, status, current_step, step_index, step_total,
                 config_json, expected_peers_json, completed_peers_json,
                 created_at, updated_at
             ) VALUES ('run-2', 'ChangeThreshold', 'Coordinator', 'inprogress', 'Submit',
                       4, 7, '{}', '[]', '[]', 0, 0)",
        )
        .execute(&pool)
        .await?;
        let state = resumed_state(&pool, vec![peer(1)]);
        let key = peer(1).to_string();
        for kind in [
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
        ] {
            pool.write_artifact("run-2", kind, Some(&key), b"sig")
                .await?;
        }

        let message = prepare_signatures(&state, &pool, "run-1")
            .await
            .expect_err("another run's signatures must not submit this run")
            .to_string();

        assert_eq!(message.matches(&key).count(), 2, "{message}");
        Ok(())
    }

    /// Complete split artifacts do not excuse a combined upload that cannot split.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_corrupt_combined_upload_stops_the_submit(pool: SqlitePool) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = resumed_state(&pool, vec![peer(1)]);
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            &peer(1),
            &[1, 11],
        )
        .await?;
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
            &peer(1),
            &[1, 22],
        )
        .await?;
        // The DNS length claims five bytes, but only one follows.
        write(
            &pool,
            artifact_kinds::CHANGE_THRESHOLD_SIGNATURE_UPLOAD,
            &peer(1),
            &[5, 11],
        )
        .await?;

        let message = format!(
            "{:#}",
            prepare_signatures(&state, &pool, "run-1")
                .await
                .expect_err("a corrupt upload must not submit")
        );

        assert!(message.contains("Failed to split"), "{message}");
        assert!(message.contains(&peer(1).to_string()), "{message}");
        Ok(())
    }

    /// The durable upload is the source of truth over older split artifacts.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_durable_upload_replaces_stale_split_artifacts(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_run(&pool).await?;
        let state = resumed_state(&pool, vec![peer(1), peer(2)]);
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            &peer(1),
            &[1, 99],
        )
        .await?;
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
            &peer(1),
            &[1, 98],
        )
        .await?;
        write(
            &pool,
            artifact_kinds::CHANGE_THRESHOLD_SIGNATURE_UPLOAD,
            &peer(1),
            &[1, 11, 1, 22],
        )
        .await?;
        // peer(2) is a legacy peer with split artifacts only.
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            &peer(2),
            &[1, 33],
        )
        .await?;
        write(
            &pool,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
            &peer(2),
            &[1, 44],
        )
        .await?;

        prepare_signatures(&state, &pool, "run-1").await?;

        for (p, dns, p2p) in [(peer(1), [1, 11], [1, 22]), (peer(2), [1, 33], [1, 44])] {
            let key = p.to_string();
            let read = |kind| pool.read_artifact("run-1", kind, Some(&key));
            assert_eq!(
                read(artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS).await?,
                Some(dns.to_vec())
            );
            assert_eq!(
                read(artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P).await?,
                Some(p2p.to_vec())
            );
        }
        Ok(())
    }
}
