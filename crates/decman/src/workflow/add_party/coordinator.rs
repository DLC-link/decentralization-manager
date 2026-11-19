use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        party_replication::{
            collect_party_package_ids, open_export_session, wait_for_flag_cleared,
        },
        signature_pairs::save_signature_pairs as save_recorded_signature_pairs,
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
    },
};

use super::{
    AddPartyConfig, AddPartyStep, resolve_ledger_token,
    steps::{create_proposals, export_state, submit_clear_proposal, submit_proposals},
};

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    add_party_config: AddPartyConfig,
    workflow_auth: Option<WorkflowAuth>,
    db: SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result {
    tracing::info!("Initializing Noise server for add-party...");

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        add_party_config.instance_name.clone(),
        AddPartyStep::WaitingForPeers,
        None, // the new member must connect — nobody is excluded
        last_seen,
    )
    .await?;
    let server = Arc::new(server);

    let workflow_state = server.get_workflow_state();
    if !matches!(
        workflow_state.current_step().await,
        AddPartyStep::SignProposals | AddPartyStep::SignClearOnboarding
    ) {
        let config_payload = serde_json::to_vec(&add_party_config)
            .context("Failed to serialize add-party config")?;
        workflow_state.set_command_payload(config_payload).await;
    }
    let node_config_clone = node_config.clone();
    let add_party_config_clone = add_party_config.clone();
    let db_clone = db.clone();
    let workflow_handle = tokio::spawn(async move {
        run_workflow(
            workflow_state,
            node_config_clone,
            add_party_config_clone,
            workflow_auth,
            db_clone,
        )
        .await
    });

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::AddParty(server),
        instance,
        workflow_handle,
    )
    .await
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<AddPartyStep>>,
    node_config: NodeConfig,
    add_party_config: AddPartyConfig,
    workflow_auth: Option<WorkflowAuth>,
    db: SqlitePool,
) -> Result {
    let instance_name = add_party_config.instance_name.clone();
    let config_payload =
        serde_json::to_vec(&add_party_config).context("Failed to serialize add-party config")?;

    loop {
        let current_step = workflow_state.current_step().await;
        tracing::debug!("Add-party coordinator in step: {current_step:?}");

        match current_step {
            AddPartyStep::WaitingForPeers
            | AddPartyStep::GenerateNewMemberKeys
            | AddPartyStep::SignProposals
            | AddPartyStep::ProposeClearOnboarding
            | AddPartyStep::SignClearOnboarding => {
                // Peer-gated (or connection-gated) — the listener advances the
                // state as peers report in; the coordinator just idles.
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            AddPartyStep::SyncAcs => {
                // Peer-gated like the arm above, but two things a resumed
                // coordinator cannot rebuild live here: the command payload,
                // which is restored from its artifact, and the Canton export
                // stream, which has to be re-opened.
                // Startup seeds the bare config. Compare payloads rather than
                // checking for emptiness so resumed commands include package IDs.
                if let Some(saved) = db
                    .read_artifact(
                        &instance_name,
                        artifact_kinds::ADD_PARTY_SYNC_ACS_COMMAND,
                        None,
                    )
                    .await?
                    && workflow_state.get_command_payload().await != saved
                {
                    tracing::info!(
                        "Restored the SyncAcs command payload after a restart ({len} bytes)",
                        len = saved.len()
                    );
                    workflow_state.set_command_payload(saved).await;
                }
                // The export is a live gRPC stream and cannot outlive the
                // process. Re-opening it lets a retried run work; the new member
                // starts again from block 1, since the stream cannot be seeked.
                if !workflow_state.has_acs_export().await {
                    tracing::warn!(
                        "No ACS export is open — re-opening it; the transfer restarts \
                         from the beginning"
                    );
                    let target = add_party_config.replication_target(&instance_name);
                    let session = open_export_session(&node_config, &db, &target).await?;
                    workflow_state
                        .set_acs_export(add_party_config.new_participant_id.clone(), session)
                        .await;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            AddPartyStep::ExportState => {
                tracing::info!("Coordinator executing: Export state");
                save_new_member_keys(&db, &instance_name, &add_party_config).await?;
                let ledger_token =
                    resolve_ledger_token(&workflow_auth, &add_party_config.decentralized_party_id)
                        .await;
                export_state(
                    &node_config,
                    &db,
                    &instance_name,
                    &add_party_config,
                    ledger_token.as_deref(),
                )
                .await?;
                workflow_state.advance_step().await;
            }
            AddPartyStep::CreateProposals => {
                tracing::info!("Coordinator executing: Create proposals");
                create_proposals(&node_config, &db, &instance_name, &add_party_config).await?;

                let dns_data = db
                    .read_artifact(&instance_name, artifact_kinds::ADD_PARTY_DNS_PROPOSAL, None)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "ADD_PARTY_DNS_PROPOSAL artifact missing after CreateProposals"
                        )
                    })?;
                let p2p_data = db
                    .read_artifact(&instance_name, artifact_kinds::ADD_PARTY_P2P_PROPOSAL, None)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "ADD_PARTY_P2P_PROPOSAL artifact missing after CreateProposals"
                        )
                    })?;

                let payload =
                    utils::encode_length_prefixed(&[&config_payload, &dns_data, &p2p_data]);
                workflow_state.set_command_payload(payload).await;
                workflow_state.advance_step().await;
            }
            AddPartyStep::SubmitProposals => {
                tracing::info!("Coordinator executing: Submit proposals");
                save_signature_pairs(&workflow_state, &db, &instance_name).await?;
                submit_proposals(&node_config, &db, &instance_name, &add_party_config).await?;
                copy_new_member_identity(&db, &instance_name, &add_party_config).await?;

                // The topology is live. Open the export but read nothing from it:
                // the new member pulls it block by block straight into its own
                // Canton import, so the snapshot is never assembled here.
                let target = add_party_config.replication_target(&instance_name);
                let session = open_export_session(&node_config, &db, &target).await?;
                workflow_state
                    .set_acs_export(add_party_config.new_participant_id.clone(), session)
                    .await;

                // Package ids the new member must have to validate the imported
                // ACS — its preflight fails fast (before disconnecting) if any
                // are missing, instead of the import dying mid-window.
                let party_id = add_party_config.decentralized_party_id.to_string();
                let ledger_token =
                    resolve_ledger_token(&workflow_auth, &add_party_config.decentralized_party_id)
                        .await;
                let package_ids =
                    collect_party_package_ids(&node_config, &party_id, ledger_token.as_deref())
                        .await?;
                let package_ids_payload = package_ids.join("\n").into_bytes();
                let payload =
                    utils::encode_length_prefixed(&[&config_payload, &package_ids_payload]);
                db.write_artifact(
                    &instance_name,
                    artifact_kinds::ADD_PARTY_SYNC_ACS_COMMAND,
                    None,
                    &payload,
                )
                .await?;
                workflow_state.set_command_payload(payload).await;
                workflow_state.advance_step().await;
            }
            AddPartyStep::PrepareClearOnboarding => {
                // SyncAcs is done, so close the Canton export stream rather
                // than holding it open for the rest of the run.
                workflow_state.clear_acs_export().await;
                // Swap the SyncAcs payload for the bare config before the next
                // peer-gated command.
                workflow_state
                    .set_command_payload(config_payload.clone())
                    .await;
                workflow_state.advance_step().await;
            }
            AddPartyStep::PrepareClearSign => {
                tracing::info!("Coordinator executing: Prepare clearing proposal");
                // The clearing proposal is AUTHORED BY THE NEW MEMBER (Canton
                // requires the onboarding participant to issue the flag-clear
                // transaction) and arrived as its ProposeClearOnboarding data
                // upload; other peers replied with skip statuses (no data).
                let uploaded = db
                    .read_artifact(
                        &instance_name,
                        artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL_UPLOAD,
                        Some(&add_party_config.new_participant_id.to_string()),
                    )
                    .await?;
                let proposal_blob = confirmed_clearing_proposal(uploaded, async {
                    let synchronizer_id = utils::get_synchronizer_id(&node_config).await?;
                    wait_for_flag_cleared(
                        &node_config,
                        &synchronizer_id,
                        &add_party_config.decentralized_party_id,
                        &add_party_config.new_participant_id,
                    )
                    .await
                })
                .await?;
                db.write_artifact(
                    &instance_name,
                    artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL,
                    None,
                    &proposal_blob,
                )
                .await?;
                let payload = utils::encode_length_prefixed(&[&config_payload, &proposal_blob]);
                workflow_state.set_command_payload(payload).await;
                workflow_state.advance_step().await;
            }
            AddPartyStep::SubmitClearOnboarding => {
                let proposal = db
                    .read_artifact(
                        &instance_name,
                        artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL,
                        None,
                    )
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Clearing proposal upload missing"))?;
                if proposal.is_empty() {
                    tracing::info!("Onboarding flag already cleared — skipping submit");
                } else {
                    tracing::info!("Coordinator executing: Submit clearing proposal");
                    save_clear_signatures(&workflow_state, &db, &instance_name).await?;
                    submit_clear_proposal(&node_config, &db, &instance_name, &add_party_config)
                        .await?;
                }
                workflow_state.advance_step().await;
            }
            AddPartyStep::RetireLegacyKeys => {
                crate::workflow::topology::retire_legacy_keys_and_wait(
                    &node_config,
                    &add_party_config.decentralized_party_id,
                )
                .await?;
                workflow_state.advance_step().await;
            }
            AddPartyStep::Complete => {
                tracing::info!("Add-party workflow complete!");
                tracing::debug!("Waiting for peers to receive Disconnect command...");
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                break;
            }
        }
    }

    Ok(())
}

/// Older members report an already-cleared flag as a status without an upload.
/// Both missing uploads and explicit empty markers require ledger confirmation.
async fn confirmed_clearing_proposal(
    uploaded: Option<Vec<u8>>,
    verify_cleared: impl std::future::Future<Output = Result>,
) -> Result<Vec<u8>> {
    match uploaded {
        Some(proposal) if !proposal.is_empty() => Ok(proposal),
        _ => {
            verify_cleared.await?;
            tracing::info!("Onboarding flag confirmed cleared — skipping sign round");
            Ok(Vec::new())
        }
    }
}

/// Rebuild both key outputs from the durable new-member upload on every retry.
pub(crate) async fn save_new_member_keys(
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    let expected = &add_party_config.new_participant_id;
    let peer_key = expected.to_string();
    let uploaded = storage
        .read_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_KEYS_UPLOAD,
            Some(&peer_key),
        )
        .await?;
    let Some(data) = uploaded else {
        let keys = storage
            .read_artifact(
                instance_name,
                artifact_kinds::PEER_PUBLIC_KEYS,
                Some(&peer_key),
            )
            .await?;
        let participant = storage
            .read_artifact(
                instance_name,
                artifact_kinds::PARTICIPANT_ID,
                Some(&peer_key),
            )
            .await?;
        anyhow::ensure!(
            keys.is_some() && participant.is_some(),
            "New member key upload missing and saved key outputs incomplete"
        );
        tracing::info!(%expected, "Recovered both saved key outputs from a legacy add-party run");
        return Ok(());
    };
    let items = utils::decode_length_prefixed(&data, 2).context("Invalid new member key upload")?;
    for (kind, payload) in [
        (artifact_kinds::PEER_PUBLIC_KEYS, &items[0]),
        (artifact_kinds::PARTICIPANT_ID, &items[1]),
    ] {
        storage
            .write_artifact(instance_name, kind, Some(&peer_key), payload)
            .await?;
    }

    Ok(())
}

/// Every invited peer signs both add-party rounds. Check the expected set,
/// because advancing the signature step clears the completed set.
async fn require_expected_peer_artifacts(
    workflow_state: &WorkflowState<AddPartyStep>,
    storage: &SqlitePool,
    instance_name: &str,
    artifact_kind: &str,
    step: &str,
) -> Result {
    let expected = workflow_state.expected_peers();
    let persisted: HashSet<String> = storage
        .list_artifacts(instance_name, artifact_kind)
        .await?
        .into_iter()
        .map(|(peer, _)| peer)
        .collect();

    let missing: Vec<String> = expected
        .iter()
        .map(std::string::ToString::to_string)
        .filter(|peer| !persisted.contains(peer))
        .collect();

    if !missing.is_empty() {
        anyhow::bail!(
            "{step}: {count} expected peer(s) have no {artifact_kind} artifact: \
             {missing:?}. A coordinator restart before this version could lose their uploads, \
             and the peers will not re-send. Start a fresh add-party run for this party.",
            count = missing.len()
        );
    }

    tracing::info!(
        "{step}: all {count} expected peer(s) have a {artifact_kind} artifact",
        count = expected.len()
    );
    Ok(())
}

/// Split each peer's combined DNS||P2P signature upload into the two
/// per-peer artefacts the submit step joins by peer id.
async fn save_signature_pairs(
    workflow_state: &WorkflowState<AddPartyStep>,
    storage: &SqlitePool,
    instance_name: &str,
) -> Result {
    save_recorded_signature_pairs(
        storage,
        instance_name,
        artifact_kinds::ADD_PARTY_SIGNATURE_UPLOAD,
        artifact_kinds::SIGNED_ADD_PARTY_DNS,
        artifact_kinds::SIGNED_ADD_PARTY_P2P,
    )
    .await?;
    for kind in [
        artifact_kinds::SIGNED_ADD_PARTY_DNS,
        artifact_kinds::SIGNED_ADD_PARTY_P2P,
    ] {
        require_expected_peer_artifacts(
            workflow_state,
            storage,
            instance_name,
            kind,
            "SubmitProposals",
        )
        .await?;
    }
    Ok(())
}

/// Check the durable clearing signatures before submission.
async fn save_clear_signatures(
    workflow_state: &WorkflowState<AddPartyStep>,
    storage: &SqlitePool,
    instance_name: &str,
) -> Result {
    require_expected_peer_artifacts(
        workflow_state,
        storage,
        instance_name,
        artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
        "SubmitClearOnboarding",
    )
    .await
}

/// Identity hook: once the topology includes the new member, copy its
/// `PEER_PUBLIC_KEYS` + `PARTICIPANT_ID` artefacts into `dec_party_identity`
/// keyed by the party — post-add workflows (kick, contracts) on this node
/// read peer identities from there, and they must include the new member.
async fn copy_new_member_identity(
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    let party_id = &add_party_config.decentralized_party_id;
    let new_member = add_party_config.new_participant_id.to_string();

    for (workflow_kind, identity_kind) in [
        (
            artifact_kinds::PEER_PUBLIC_KEYS,
            identity_kinds::PEER_PUBLIC_KEYS,
        ),
        (
            artifact_kinds::PARTICIPANT_ID,
            identity_kinds::PARTICIPANT_ID,
        ),
    ] {
        if let Some(payload) = storage
            .read_artifact(instance_name, workflow_kind, Some(&new_member))
            .await?
        {
            storage
                .write_identity(party_id, identity_kind, &new_member, &payload)
                .await?;
        }
    }
    tracing::info!("Persisted new member identity for {party_id}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::{
        canton_id::{CantonId, NAMESPACE_LENGTH, Namespace},
        db::MIGRATOR,
    };

    fn peer(n: u8) -> CantonId {
        CantonId::new(format!("p{n}"), Namespace::new([n; NAMESPACE_LENGTH]))
    }

    /// `workflow_artifacts.instance_name` is a foreign key onto
    /// `workflow_runs`, so an artefact needs its run row to exist first.
    async fn insert_run(pool: &SqlitePool, instance_name: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO workflow_runs (
                 instance_name, kind, role, status, current_step, step_index, step_total,
                 config_json, expected_peers_json, completed_peers_json,
                 created_at, updated_at
             ) VALUES (?1, 'AddParty', 'Coordinator', 'inprogress', 'SubmitProposals',
                       0, 1, '{}', '[]', '[]', 0, 0)",
        )
        .bind(instance_name)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// A resumed coordinator: `peer_data` is empty, `completed_peers` is not.
    fn resumed_state(
        pool: &SqlitePool,
        completed: Vec<CantonId>,
    ) -> Arc<WorkflowState<AddPartyStep>> {
        WorkflowState::from_persisted(
            pool.clone(),
            "run-1".to_string(),
            AddPartyStep::SubmitProposals,
            completed.clone(),
            completed,
            None,
        )
    }

    /// The window this guards: peers uploaded, the peer-gated step advanced and
    /// persisted, then the coordinator restarted before the artefacts were
    /// written. `peer_data` comes back empty and the peers stay `completed`, so
    /// nothing will re-send.
    ///
    /// Before the guard this returned `Ok(())` and the run submitted with only
    /// the coordinator's own signature.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn completed_peers_with_no_artefacts_fail_loudly(pool: SqlitePool) -> anyhow::Result<()> {
        insert_run(&pool, "run-1").await?;
        let state = resumed_state(&pool, vec![peer(1), peer(2)]);

        let err = require_expected_peer_artifacts(
            &state,
            &pool,
            "run-1",
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            "SubmitProposals",
        )
        .await
        .expect_err("completed peers with no artefacts must not pass");

        let msg = format!("{err}");
        assert!(
            msg.contains("coordinator restart"),
            "unhelpful error: {msg}"
        );
        assert!(msg.contains("will not"), "unhelpful error: {msg}");
        Ok(())
    }

    /// A PARTIAL set must fail too. Artefacts are written per peer and each
    /// write commits on its own, so a restart mid-loop leaves some peers
    /// persisted and some not. Treating "not empty" as good enough would let
    /// the run submit under-signed anyway, which is the failure this guard
    /// exists to stop.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_partial_artefact_set_fails(pool: SqlitePool) -> anyhow::Result<()> {
        insert_run(&pool, "run-1").await?;
        let state = resumed_state(&pool, vec![peer(1), peer(2)]);

        // Only peer(1) made it to disk before the restart.
        pool.write_artifact(
            "run-1",
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            Some(&peer(1).to_string()),
            b"sig",
        )
        .await?;

        let err = require_expected_peer_artifacts(
            &state,
            &pool,
            "run-1",
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            "SubmitProposals",
        )
        .await
        .expect_err("a partial artefact set must not pass");

        let msg = format!("{err}");
        assert!(
            msg.contains(&peer(2).to_string()),
            "must name who is missing: {msg}"
        );
        assert!(
            !msg.contains(&peer(1).to_string()),
            "must not name the peer it has: {msg}"
        );
        Ok(())
    }

    /// The legitimate resume: every completed peer already has its artefact, so
    /// re-entering the step with an empty buffer continues. Without this the
    /// guard would break every resumed run.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_complete_artefact_set_continues(pool: SqlitePool) -> anyhow::Result<()> {
        insert_run(&pool, "run-1").await?;
        let state = resumed_state(&pool, vec![peer(1), peer(2)]);

        for p in [peer(1), peer(2)] {
            pool.write_artifact(
                "run-1",
                artifact_kinds::SIGNED_ADD_PARTY_DNS,
                Some(&p.to_string()),
                b"sig",
            )
            .await?;
        }

        require_expected_peer_artifacts(
            &state,
            &pool,
            "run-1",
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            "SubmitProposals",
        )
        .await?;
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_resumed_submit_checks_peers_after_completions_are_cleared(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_run(&pool, "run-1").await?;
        let state = WorkflowState::from_persisted(
            pool.clone(),
            "run-1".into(),
            AddPartyStep::SubmitProposals,
            vec![peer(1)],
            vec![],
            None,
        );
        let err = save_signature_pairs(&state, &pool, "run-1")
            .await
            .expect_err("missing legacy uploads must not permit an under-signed submit");
        assert!(err.to_string().contains(&peer(1).to_string()));

        let combined = [1, 11, 1, 22];
        pool.write_artifact(
            "run-1",
            artifact_kinds::ADD_PARTY_SIGNATURE_UPLOAD,
            Some(&peer(1).to_string()),
            &combined,
        )
        .await?;
        save_signature_pairs(&state, &pool, "run-1").await?;
        assert_eq!(
            pool.read_artifact(
                "run-1",
                artifact_kinds::SIGNED_ADD_PARTY_P2P,
                Some(&peer(1).to_string()),
            )
            .await?,
            Some(vec![1, 22])
        );
        Ok(())
    }

    /// The two steps key on different artefact kinds, so a run that persisted
    /// DNS signatures must not thereby satisfy the clearing-signature guard.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_guard_is_per_artefact_kind(pool: SqlitePool) -> anyhow::Result<()> {
        insert_run(&pool, "run-1").await?;
        let state = resumed_state(&pool, vec![peer(1)]);

        pool.write_artifact(
            "run-1",
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            Some(&peer(1).to_string()),
            b"sig",
        )
        .await?;

        assert!(
            require_expected_peer_artifacts(
                &state,
                &pool,
                "run-1",
                artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
                "SubmitClearOnboarding",
            )
            .await
            .is_err(),
            "DNS artefacts must not satisfy the clearing-signature guard"
        );
        Ok(())
    }

    #[tokio::test]
    async fn missing_clearing_upload_requires_confirmed_ledger_state() -> anyhow::Result<()> {
        let cleared = confirmed_clearing_proposal(None, async { Ok(()) }).await?;
        assert!(cleared.is_empty());
        let error = confirmed_clearing_proposal(None, async {
            anyhow::bail!("Onboarding flag remains set")
        })
        .await
        .expect_err("an older member's status cannot bypass the ledger check");
        assert!(error.to_string().contains("flag remains set"));
        Ok(())
    }

    #[tokio::test]
    async fn explicit_clearing_marker_still_requires_confirmation() -> anyhow::Result<()> {
        let cleared = confirmed_clearing_proposal(Some(Vec::new()), async { Ok(()) }).await?;
        assert!(cleared.is_empty());
        assert!(
            confirmed_clearing_proposal(Some(Vec::new()), async {
                anyhow::bail!("Onboarding flag remains set")
            })
            .await
            .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn saved_proposal_does_not_skip_or_poll_for_a_cleared_flag() -> anyhow::Result<()> {
        use canton_proto_rs::com::digitalasset::canton::protocol::v30::SignedTopologyTransaction;
        let proposal = utils::encode_length_prefixed_message(&SignedTopologyTransaction::default());
        let restored = confirmed_clearing_proposal(Some(proposal.clone()), async {
            panic!("a proposal must enter signing without waiting for its future effect")
        })
        .await?;
        assert_eq!(restored, proposal);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn legacy_key_recovery_requires_both_outputs(pool: SqlitePool) -> anyhow::Result<()> {
        for (index, keys, participant) in [
            (0, true, true),
            (1, true, false),
            (2, false, true),
            (3, false, false),
        ] {
            let instance = format!("legacy-{index}");
            insert_run(&pool, &instance).await?;
            let config = AddPartyConfig::new(peer(9), peer(1), 1, None, 1, instance.clone());
            let member = peer(1).to_string();
            if keys {
                pool.write_artifact(
                    &instance,
                    artifact_kinds::PEER_PUBLIC_KEYS,
                    Some(&member),
                    b"saved-keys",
                )
                .await?;
            }
            if participant {
                pool.write_artifact(
                    &instance,
                    artifact_kinds::PARTICIPANT_ID,
                    Some(&member),
                    b"saved-participant",
                )
                .await?;
            }
            let recovered = save_new_member_keys(&pool, &instance, &config).await;
            assert_eq!(recovered.is_ok(), keys && participant);
            assert_eq!(
                pool.read_artifact(&instance, artifact_kinds::PEER_PUBLIC_KEYS, Some(&member))
                    .await?
                    .as_deref(),
                keys.then_some(b"saved-keys".as_slice())
            );
            assert_eq!(
                pool.read_artifact(&instance, artifact_kinds::PARTICIPANT_ID, Some(&member))
                    .await?
                    .as_deref(),
                participant.then_some(b"saved-participant".as_slice())
            );
        }
        Ok(())
    }
}
