use std::{collections::HashMap, sync::Arc, time::Duration};

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use super::*;
use crate::{
    canton_id::{NAMESPACE_LENGTH, Namespace},
    config::{NetworkConfig, NodeConfig},
    db::MIGRATOR,
    error::Result,
    noise::NoiseKeypair,
    server::{WorkflowInstance, WorkflowRole},
    workflow::{
        add_party::{
            AddPartyConfig, AddPartyStep,
            coordinator::start_coordinator as start_add_party_coordinator,
        },
        change_threshold::ChangeThresholdStep,
        kick::KickStep,
        signature_pairs::save_signature_pairs,
        storage::WorkflowStorage,
    },
};

const RUN: &str = "signature-run";
const CONFIG: &[u8] = br#"{"instance_name":"signature-run","marker":"saved-config"}"#;
const PAIR: &[u8] = &[2, 11, 12, 3, 21, 22, 23];

fn peer(n: u8) -> CantonId {
    CantonId::new(format!("p{n}"), Namespace::new([n; NAMESPACE_LENGTH]))
}

async fn insert_run<S: WorkflowStep>(db: &SqlitePool, step: S) -> Result {
    sqlx::query(
        "INSERT INTO workflow_runs
         (instance_name, kind, role, status, current_step, step_index, step_total,
          config_json, expected_peers_json, completed_peers_json, created_at, updated_at)
         VALUES (?, ?, 'Coordinator', 'inprogress', ?, ?, ?, ?, ?, '[]', 0, 0)",
    )
    .bind(RUN)
    .bind(S::kind().as_str())
    .bind(step.step_name())
    .bind(step.step_index())
    .bind(S::step_total())
    .bind(std::str::from_utf8(CONFIG)?)
    .bind(serde_json::to_string(&[peer(1), peer(2)])?)
    .execute(db)
    .await?;
    for (kind, proposal) in proposal_artifacts(step) {
        db.write_artifact(RUN, kind, None, proposal).await?;
    }
    Ok(())
}

fn proposal_artifacts<S: WorkflowStep>(step: S) -> Vec<(&'static str, &'static [u8])> {
    match step.to_command() {
        Some(MessageType::SignKick) => vec![
            (artifact_kinds::KICK_DNS_PROPOSAL, b"saved DNS"),
            (artifact_kinds::KICK_P2P_PROPOSAL, b"saved P2P"),
        ],
        Some(MessageType::SignChangeThreshold) => vec![
            (artifact_kinds::CHANGE_THRESHOLD_DNS_PROPOSAL, b"saved DNS"),
            (artifact_kinds::CHANGE_THRESHOLD_P2P_PROPOSAL, b"saved P2P"),
        ],
        Some(MessageType::SignAddParty) => vec![
            (artifact_kinds::ADD_PARTY_DNS_PROPOSAL, b"saved DNS"),
            (artifact_kinds::ADD_PARTY_P2P_PROPOSAL, b"saved P2P"),
        ],
        Some(MessageType::SignClearOnboarding) => {
            vec![(artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL, b"saved clear")]
        }
        _ => vec![],
    }
}

async fn start<S: WorkflowStep + 'static>(
    db: &SqlitePool,
    root: &tempfile::TempDir,
    step: S,
) -> Result<NoiseServer<S>> {
    let config = NodeConfig::default().with_root_dir(root.path());
    if !config.key_file_path().exists() {
        tokio::fs::create_dir_all(config.data_dir()).await?;
        NoiseKeypair::generate()
            .save_to_file(config.key_file_path())
            .await?;
    }
    Ok(NoiseServer::new(
        config,
        NetworkConfig::default(),
        db.clone(),
        RUN.into(),
        step,
        None,
        Arc::new(RwLock::new(HashMap::new())),
    )
    .await?)
}

async fn send<S: WorkflowStep + 'static>(
    server: &NoiseServer<S>,
    from: u8,
    msg_type: MessageType,
    payload: &[u8],
) -> Result<MessageType> {
    Ok(server
        .handle_command(peer(from), Message::new(msg_type, payload.to_vec()))
        .await?
        .msg_type)
}

async fn survives_restart<S: WorkflowStep + 'static>(
    db: SqlitePool,
    step: S,
    msg_type: MessageType,
    upload_kind: &str,
    split_kinds: Option<(&str, &str)>,
) -> Result {
    let root = tempfile::tempdir()?;
    insert_run(&db, step).await?;
    let server = start(&db, &root, step).await?;
    assert_eq!(send(&server, 1, msg_type, PAIR).await?, MessageType::Ack);
    assert_eq!(server.workflow_state.current_step().await, step);
    assert_eq!(
        db.read_artifact(RUN, upload_kind, Some(&peer(1).to_string()))
            .await?,
        Some(PAIR.to_vec())
    );
    drop(server);

    let server = start(&db, &root, step).await?;
    assert!(server.workflow_state.get_all_peer_data().await.is_empty());
    assert!(server.workflow_state.has_peer_completed(&peer(1)).await);
    assert_eq!(
        send(&server, 1, MessageType::GetNextCommand, &[]).await?,
        MessageType::Wait
    );
    let command = server
        .handle_command(peer(2), Message::new_empty(MessageType::GetNextCommand))
        .await?;
    assert_eq!(Some(command.msg_type), step.to_command());
    let proposals = proposal_artifacts(step);
    let items = utils::decode_length_prefixed(&command.payload, proposals.len() + 1)?;
    assert_eq!(items[0], CONFIG);
    for (item, (_, proposal)) in items[1..].iter().zip(proposals) {
        assert_eq!(item, proposal);
    }
    assert_eq!(send(&server, 2, msg_type, PAIR).await?, MessageType::Ack);
    assert_eq!(
        server.workflow_state.current_step().await,
        step.next().unwrap()
    );
    drop(server);

    let server = start(&db, &root, step).await?;
    assert_eq!(
        server.workflow_state.current_step().await,
        step.next().unwrap()
    );
    assert!(server.workflow_state.get_all_peer_data().await.is_empty());
    if let Some((dns_kind, p2p_kind)) = split_kinds {
        // A process may also stop after writing only one split artifact.
        db.write_artifact(RUN, dns_kind, Some(&peer(1).to_string()), &[2, 11, 12])
            .await?;
        save_signature_pairs(&db, RUN, upload_kind, dns_kind, p2p_kind).await?;
        for p in [peer(1), peer(2)] {
            let key = p.to_string();
            assert_eq!(
                db.read_artifact(RUN, dns_kind, Some(&key)).await?,
                Some(vec![2, 11, 12])
            );
            assert_eq!(
                db.read_artifact(RUN, p2p_kind, Some(&key)).await?,
                Some(vec![3, 21, 22, 23])
            );
        }
    } else {
        assert_eq!(db.list_artifacts(RUN, upload_kind).await?.len(), 2);
    }
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn kick_signatures_survive_restart(db: SqlitePool) -> Result {
    survives_restart(
        db,
        KickStep::SignProposals,
        MessageType::KickSignatures,
        artifact_kinds::KICK_SIGNATURE_UPLOAD,
        Some((
            artifact_kinds::SIGNED_KICK_DNS,
            artifact_kinds::SIGNED_KICK_P2P,
        )),
    )
    .await
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn change_threshold_signatures_survive_restart(db: SqlitePool) -> Result {
    survives_restart(
        db,
        ChangeThresholdStep::SignProposals,
        MessageType::ChangeThresholdSignatures,
        artifact_kinds::CHANGE_THRESHOLD_SIGNATURE_UPLOAD,
        Some((
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
        )),
    )
    .await
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn add_party_signatures_survive_restart(db: SqlitePool) -> Result {
    survives_restart(
        db,
        AddPartyStep::SignProposals,
        MessageType::AddPartySignatures,
        artifact_kinds::ADD_PARTY_SIGNATURE_UPLOAD,
        Some((
            artifact_kinds::SIGNED_ADD_PARTY_DNS,
            artifact_kinds::SIGNED_ADD_PARTY_P2P,
        )),
    )
    .await
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn clearing_signatures_survive_restart(db: SqlitePool) -> Result {
    survives_restart(
        db,
        AddPartyStep::SignClearOnboarding,
        MessageType::AddPartyClearSignatures,
        artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
        None,
    )
    .await
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_failed_write_leaves_the_peer_incomplete(db: SqlitePool) -> Result {
    let root = tempfile::tempdir()?;
    insert_run(&db, KickStep::SignProposals).await?;
    let server = start(&db, &root, KickStep::SignProposals).await?;
    sqlx::query("CREATE TRIGGER fail_upload BEFORE INSERT ON workflow_artifacts BEGIN SELECT RAISE(FAIL, 'disk failure'); END")
        .execute(&db)
        .await?;
    assert_eq!(
        send(&server, 1, MessageType::KickSignatures, PAIR).await?,
        MessageType::Error
    );
    assert!(!server.workflow_state.has_peer_completed(&peer(1)).await);
    assert_eq!(
        server.workflow_state.current_step().await,
        KickStep::SignProposals
    );
    assert!(
        db.list_artifacts(RUN, artifact_kinds::KICK_SIGNATURE_UPLOAD)
            .await?
            .is_empty()
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn invalid_unexpected_and_late_uploads_do_not_complete_a_step(db: SqlitePool) -> Result {
    let root = tempfile::tempdir()?;
    insert_run(&db, KickStep::SignProposals).await?;
    let server = start(&db, &root, KickStep::SignProposals).await?;
    assert_eq!(
        send(&server, 1, MessageType::KickSignatures, &[5, 1]).await?,
        MessageType::Error
    );
    let unexpected = server
        .handle_command(
            peer(9),
            Message::new(MessageType::KickSignatures, PAIR.to_vec()),
        )
        .await?;
    assert_eq!(unexpected.msg_type, MessageType::Error);
    assert_eq!(
        unexpected.payload,
        b"The peer is not expected in this workflow"
    );
    assert_eq!(
        send(&server, 1, MessageType::AddPartySignatures, PAIR).await?,
        MessageType::Ack
    );
    assert!(server.workflow_state.completed_peers().await.is_empty());
    assert!(
        db.list_artifacts(RUN, artifact_kinds::KICK_SIGNATURE_UPLOAD)
            .await?
            .is_empty()
    );
    assert!(
        db.list_artifacts(RUN, artifact_kinds::ADD_PARTY_SIGNATURE_UPLOAD)
            .await?
            .is_empty()
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn an_upload_retries_after_storage_recovers(db: SqlitePool) -> Result {
    let root = tempfile::tempdir()?;
    insert_run(&db, AddPartyStep::SignClearOnboarding).await?;
    let server = start(&db, &root, AddPartyStep::SignClearOnboarding).await?;
    sqlx::query("CREATE TRIGGER reject_signature BEFORE INSERT ON workflow_artifacts BEGIN SELECT RAISE(FAIL, 'unavailable'); END")
        .execute(&db)
        .await?;
    assert_eq!(
        send(
            &server,
            1,
            MessageType::AddPartyClearSignatures,
            b"clearing signature"
        )
        .await?,
        MessageType::Error
    );
    drop(server);
    sqlx::query("DROP TRIGGER reject_signature")
        .execute(&db)
        .await?;

    let server = start(&db, &root, AddPartyStep::SignClearOnboarding).await?;
    assert!(!server.workflow_state.has_peer_completed(&peer(1)).await);
    assert_eq!(
        send(
            &server,
            1,
            MessageType::AddPartyClearSignatures,
            b"clearing signature"
        )
        .await?,
        MessageType::Ack
    );
    drop(server);
    let server = start(&db, &root, AddPartyStep::SignClearOnboarding).await?;
    assert!(server.workflow_state.has_peer_completed(&peer(1)).await);
    assert_eq!(
        db.read_artifact(
            RUN,
            artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
            Some(&peer(1).to_string())
        )
        .await?,
        Some(b"clearing signature".to_vec())
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn submission_preserves_distinct_peer_pairs_and_ignores_a_late_replacement(
    db: SqlitePool,
) -> Result {
    let root = tempfile::tempdir()?;
    let other_pair = [1, 31, 2, 41, 42];
    insert_run(&db, ChangeThresholdStep::SignProposals).await?;
    let server = start(&db, &root, ChangeThresholdStep::SignProposals).await?;
    assert_eq!(
        send(&server, 1, MessageType::ChangeThresholdSignatures, PAIR).await?,
        MessageType::Ack
    );
    drop(server);
    let server = start(&db, &root, ChangeThresholdStep::SignProposals).await?;
    assert_eq!(
        send(
            &server,
            2,
            MessageType::ChangeThresholdSignatures,
            &other_pair
        )
        .await?,
        MessageType::Ack
    );
    assert_eq!(
        server.workflow_state.current_step().await,
        ChangeThresholdStep::Submit
    );
    // An acknowledgement can be lost after the step advances.
    assert_eq!(
        send(
            &server,
            1,
            MessageType::ChangeThresholdSignatures,
            &other_pair
        )
        .await?,
        MessageType::Ack
    );
    drop(server);

    for _ in 0..2 {
        save_signature_pairs(
            &db,
            RUN,
            artifact_kinds::CHANGE_THRESHOLD_SIGNATURE_UPLOAD,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS,
            artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P,
        )
        .await?;
        for (from, dns, p2p) in [
            (1, vec![2, 11, 12], vec![3, 21, 22, 23]),
            (2, vec![1, 31], vec![2, 41, 42]),
        ] {
            let key = peer(from).to_string();
            assert_eq!(
                db.read_artifact(RUN, artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS, Some(&key))
                    .await?,
                Some(dns)
            );
            assert_eq!(
                db.read_artifact(RUN, artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P, Some(&key))
                    .await?,
                Some(p2p)
            );
        }
        assert_eq!(
            db.list_artifacts(RUN, artifact_kinds::SIGNED_CHANGE_THRESHOLD_DNS)
                .await?
                .len(),
            2
        );
        assert_eq!(
            db.list_artifacts(RUN, artifact_kinds::SIGNED_CHANGE_THRESHOLD_P2P)
                .await?
                .len(),
            2
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn an_upload_survives_a_failed_progress_write_and_replay(db: SqlitePool) -> Result {
    let root = tempfile::tempdir()?;
    insert_run(&db, KickStep::SignProposals).await?;
    let server = start(&db, &root, KickStep::SignProposals).await?;
    sqlx::query("CREATE TRIGGER reject_progress BEFORE UPDATE ON workflow_runs BEGIN SELECT RAISE(FAIL, 'unavailable'); END")
        .execute(&db)
        .await?;
    assert_eq!(
        send(&server, 1, MessageType::KickSignatures, PAIR).await?,
        MessageType::Ack
    );
    let saved = SchemaRead::get_workflow_run(&db, RUN).await?.unwrap();
    assert!(saved.completed_peers.is_empty());
    assert_eq!(
        db.read_artifact(
            RUN,
            artifact_kinds::KICK_SIGNATURE_UPLOAD,
            Some(&peer(1).to_string())
        )
        .await?,
        Some(PAIR.to_vec())
    );
    drop(server);
    sqlx::query("DROP TRIGGER reject_progress")
        .execute(&db)
        .await?;

    let server = start(&db, &root, KickStep::SignProposals).await?;
    assert!(!server.workflow_state.has_peer_completed(&peer(1)).await);
    assert_eq!(
        send(&server, 1, MessageType::KickSignatures, PAIR).await?,
        MessageType::Ack
    );
    assert_eq!(
        send(&server, 2, MessageType::KickSignatures, PAIR).await?,
        MessageType::Ack
    );
    assert_eq!(
        server.workflow_state.current_step().await,
        KickStep::SubmitKick
    );
    assert_eq!(
        db.list_artifacts(RUN, artifact_kinds::KICK_SIGNATURE_UPLOAD)
            .await?
            .len(),
        2
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_missing_signature_proposal_refuses_startup(db: SqlitePool) -> Result {
    let root = tempfile::tempdir()?;
    insert_run(&db, KickStep::SignProposals).await?;
    sqlx::query("DELETE FROM workflow_artifacts WHERE artifact_kind = ?")
        .bind(artifact_kinds::KICK_P2P_PROPOSAL)
        .execute(&db)
        .await?;
    let err = match start(&db, &root, KickStep::SignProposals).await {
        Ok(_) => anyhow::bail!("a missing proposal must refuse startup"),
        Err(err) => err,
    };
    assert!(
        err.to_string()
            .contains("kick_p2p_proposal artifact missing")
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn an_empty_clearing_marker_survives_but_a_missing_marker_refuses_startup(
    db: SqlitePool,
) -> Result {
    let root = tempfile::tempdir()?;
    insert_run(&db, AddPartyStep::SignClearOnboarding).await?;
    db.write_artifact(RUN, artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL, None, &[])
        .await?;
    let server = start(&db, &root, AddPartyStep::SignClearOnboarding).await?;
    let command = server
        .handle_command(peer(1), Message::new_empty(MessageType::GetNextCommand))
        .await?;
    assert_eq!(command.msg_type, MessageType::SignClearOnboarding);
    assert_eq!(
        utils::decode_length_prefixed(&command.payload, 2)?,
        vec![CONFIG.to_vec(), vec![]]
    );
    drop(server);
    sqlx::query("DELETE FROM workflow_artifacts WHERE artifact_kind = ?")
        .bind(artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL)
        .execute(&db)
        .await?;
    let err = match start(&db, &root, AddPartyStep::SignClearOnboarding).await {
        Ok(_) => anyhow::bail!("a missing marker must refuse startup"),
        Err(err) => err,
    };
    assert!(
        err.to_string()
            .contains("add_party_clear_proposal artifact missing")
    );
    Ok(())
}

async fn production_add_party_command(
    db: SqlitePool,
    step: AddPartyStep,
) -> Result<(Message, Vec<u8>)> {
    let root = tempfile::tempdir()?;
    insert_run(&db, step).await?;
    // Preserve insignificant JSON whitespace to prove the resumed command uses
    // the persisted bytes rather than a reserialized startup argument.
    let config = AddPartyConfig::new(peer(7), peer(2), 2, None, 1, RUN.into());
    let saved_config = serde_json::to_vec_pretty(&config)?;
    sqlx::query("UPDATE workflow_runs SET config_json = ? WHERE instance_name = ?")
        .bind(std::str::from_utf8(&saved_config)?)
        .bind(RUN)
        .execute(&db)
        .await?;
    let initialized = start(&db, &root, step).await?;
    drop(initialized);
    let instance = WorkflowInstance::new(
        RUN.into(),
        WorkflowKind::AddParty,
        WorkflowRole::Coordinator,
    );
    let coordinating = tokio::spawn(start_add_party_coordinator(
        NodeConfig::default().with_root_dir(root.path()),
        NetworkConfig::default(),
        config.clone(),
        None,
        db,
        Arc::new(RwLock::new(HashMap::new())),
        instance.clone(),
    ));
    let active = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let active = instance.active.read().unwrap().clone();
            if let Some(active) = active {
                break active;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let command = if step == AddPartyStep::WaitingForPeers {
        active
            .handle_command(peer(1), Message::new_empty(MessageType::GetNextCommand))
            .await?;
        active
            .handle_command(peer(2), Message::new_empty(MessageType::GetNextCommand))
            .await?
    } else {
        active
            .handle_command(peer(1), Message::new_empty(MessageType::GetNextCommand))
            .await?
    };
    // A later poll must retain the same command payload.
    tokio::time::sleep(Duration::from_millis(10)).await;
    let repeated = active
        .handle_command(peer(1), Message::new_empty(MessageType::GetNextCommand))
        .await?;
    assert_eq!(repeated.msg_type, command.msg_type);
    assert_eq!(repeated.payload, command.payload);
    coordinating.abort();
    let _ = coordinating.await;
    let expected_config = if step == AddPartyStep::WaitingForPeers {
        serde_json::to_vec(&config)?
    } else {
        saved_config
    };
    Ok((command, expected_config))
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn production_add_party_start_preserves_the_restored_pair_command(db: SqlitePool) -> Result {
    let (command, saved_config) =
        production_add_party_command(db, AddPartyStep::SignProposals).await?;
    assert_eq!(command.msg_type, MessageType::SignAddParty);
    assert_eq!(
        utils::decode_length_prefixed(&command.payload, 3)?,
        vec![saved_config, b"saved DNS".to_vec(), b"saved P2P".to_vec()]
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn production_add_party_start_preserves_the_restored_clearing_command(
    db: SqlitePool,
) -> Result {
    let (command, saved_config) =
        production_add_party_command(db, AddPartyStep::SignClearOnboarding).await?;
    assert_eq!(command.msg_type, MessageType::SignClearOnboarding);
    assert_eq!(
        utils::decode_length_prefixed(&command.payload, 2)?,
        vec![saved_config, b"saved clear".to_vec()]
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn production_add_party_start_still_seeds_the_first_key_command(db: SqlitePool) -> Result {
    let (command, config) = production_add_party_command(db, AddPartyStep::WaitingForPeers).await?;
    assert_eq!(command.msg_type, MessageType::GenerateAddPartyKeys);
    assert_eq!(command.payload, config);
    Ok(())
}

async fn add_party_upload_run(db: &SqlitePool, step: AddPartyStep) -> Result<AddPartyConfig> {
    insert_run(db, step).await?;
    let config = AddPartyConfig::new(peer(9), peer(1), 1, None, 1, RUN.into());
    sqlx::query("UPDATE workflow_runs SET config_json = ? WHERE instance_name = ?")
        .bind(serde_json::to_string(&config)?)
        .bind(RUN)
        .execute(db)
        .await?;
    Ok(config)
}

async fn add_party_upload_restart(
    db: SqlitePool,
    step: AddPartyStep,
    msg_type: MessageType,
    upload_kind: &str,
    payload: Vec<u8>,
) -> Result {
    use crate::workflow::add_party::coordinator::save_new_member_keys;
    let root = tempfile::tempdir()?;
    let config = add_party_upload_run(&db, step).await?;
    let server = start(&db, &root, step).await?;
    assert_eq!(
        send(&server, 2, msg_type, &payload).await?,
        MessageType::Error
    );
    assert!(!server.workflow_state.has_peer_completed(&peer(2)).await);
    assert_eq!(
        send(&server, 1, msg_type, &payload).await?,
        MessageType::Ack
    );
    let member = peer(1).to_string();
    assert_eq!(
        db.read_artifact(RUN, upload_kind, Some(&member)).await?,
        Some(payload.clone())
    );
    drop(server);

    // Restore before the other member's skip status completes this handoff.
    let server = start(&db, &root, step).await?;
    assert!(server.workflow_state.has_peer_completed(&peer(1)).await);
    assert_eq!(
        send(&server, 2, MessageType::StatusUpdate, b"skip").await?,
        MessageType::Ack
    );
    assert_eq!(
        server.workflow_state.current_step().await,
        step.next().unwrap()
    );
    drop(server);
    let server = start(&db, &root, step).await?;
    assert_eq!(
        server.workflow_state.current_step().await,
        step.next().unwrap()
    );
    assert_eq!(
        db.read_artifact(RUN, upload_kind, Some(&member)).await?,
        Some(payload.clone())
    );
    if msg_type == MessageType::AddPartyKeysUpload {
        // A crash after writing only the first output must not lose the second.
        db.write_artifact(
            RUN,
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&member),
            b"partial",
        )
        .await?;
        drop(server);
        let _server = start(&db, &root, step).await?;
        save_new_member_keys(&db, RUN, &config).await?;
        assert_eq!(
            db.read_artifact(RUN, artifact_kinds::PEER_PUBLIC_KEYS, Some(&member))
                .await?,
            Some(b"keys".to_vec())
        );
        assert_eq!(
            db.read_artifact(RUN, artifact_kinds::PARTICIPANT_ID, Some(&member))
                .await?,
            Some(b"participant".to_vec())
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn add_party_keys_survive_restart_and_partial_outputs(db: SqlitePool) -> Result {
    add_party_upload_restart(
        db,
        AddPartyStep::GenerateNewMemberKeys,
        MessageType::AddPartyKeysUpload,
        artifact_kinds::ADD_PARTY_KEYS_UPLOAD,
        utils::encode_length_prefixed(&[b"keys", b"participant"]),
    )
    .await
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn add_party_clear_proposal_survives_restart(db: SqlitePool) -> Result {
    add_party_upload_restart(
        db,
        AddPartyStep::ProposeClearOnboarding,
        MessageType::AddPartyClearProposal,
        artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL_UPLOAD,
        utils::encode_length_prefixed_message(&SignedTopologyTransaction::default()),
    )
    .await
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn add_party_cleared_marker_survives_restart(db: SqlitePool) -> Result {
    add_party_upload_restart(
        db,
        AddPartyStep::ProposeClearOnboarding,
        MessageType::AddPartyClearProposal,
        artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL_UPLOAD,
        Vec::new(),
    )
    .await
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn add_party_clear_rejects_malformed_and_conflicting_uploads(db: SqlitePool) -> Result {
    let root = tempfile::tempdir()?;
    add_party_upload_run(&db, AddPartyStep::ProposeClearOnboarding).await?;
    let server = start(&db, &root, AddPartyStep::ProposeClearOnboarding).await?;
    assert_eq!(
        send(&server, 1, MessageType::AddPartyClearProposal, b"malformed").await?,
        MessageType::Error
    );
    assert!(!server.workflow_state.has_peer_completed(&peer(1)).await);
    assert_eq!(
        db.read_artifact(
            RUN,
            artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL_UPLOAD,
            Some(&peer(1).to_string())
        )
        .await?,
        None
    );
    let proposal = utils::encode_length_prefixed_message(&SignedTopologyTransaction::default());
    assert_eq!(
        send(&server, 1, MessageType::AddPartyClearProposal, &proposal).await?,
        MessageType::Ack
    );
    assert_eq!(
        send(&server, 1, MessageType::AddPartyClearProposal, &proposal).await?,
        MessageType::Ack
    );
    let conflict = server
        .handle_command(
            peer(1),
            Message::new_empty(MessageType::AddPartyClearProposal),
        )
        .await?;
    assert_eq!(conflict.msg_type, MessageType::Error);
    assert!(
        std::str::from_utf8(&conflict.payload)?.contains("Upload conflicts with saved payload")
    );
    assert_eq!(
        db.read_artifact(
            RUN,
            artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL_UPLOAD,
            Some(&peer(1).to_string())
        )
        .await?,
        Some(proposal)
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn add_party_keys_reject_malformed_conflicting_and_late_uploads(db: SqlitePool) -> Result {
    let root = tempfile::tempdir()?;
    let step = AddPartyStep::GenerateNewMemberKeys;
    add_party_upload_run(&db, step).await?;
    let server = start(&db, &root, step).await?;
    assert_eq!(
        send(&server, 1, MessageType::AddPartyKeysUpload, b"malformed").await?,
        MessageType::Error
    );
    assert!(!server.workflow_state.has_peer_completed(&peer(1)).await);
    let first = utils::encode_length_prefixed(&[b"keys", b"participant"]);
    let second = utils::encode_length_prefixed(&[b"other", b"participant"]);
    let (a, b) = tokio::join!(
        send(&server, 1, MessageType::AddPartyKeysUpload, &first),
        send(&server, 1, MessageType::AddPartyKeysUpload, &second)
    );
    let (a, b) = (a?, b?);
    assert!(matches!(
        (a, b),
        (MessageType::Ack, MessageType::Error) | (MessageType::Error, MessageType::Ack)
    ));
    let saved = db
        .read_artifact(
            RUN,
            artifact_kinds::ADD_PARTY_KEYS_UPLOAD,
            Some(&peer(1).to_string()),
        )
        .await?
        .unwrap();
    assert!(saved == first || saved == second);
    assert_eq!(
        send(&server, 1, MessageType::AddPartyKeysUpload, &saved).await?,
        MessageType::Ack
    );
    send(&server, 2, MessageType::StatusUpdate, b"skip").await?;
    assert_eq!(
        server.workflow_state.current_step().await,
        AddPartyStep::ExportState
    );
    assert_eq!(
        send(&server, 1, MessageType::AddPartyKeysUpload, b"late").await?,
        MessageType::Ack
    );
    assert_eq!(
        db.read_artifact(
            RUN,
            artifact_kinds::ADD_PARTY_KEYS_UPLOAD,
            Some(&peer(1).to_string())
        )
        .await?,
        Some(saved)
    );
    assert_eq!(
        server.workflow_state.current_step().await,
        AddPartyStep::ExportState
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn resumed_coordinator_prepares_saved_new_member_proposal(db: SqlitePool) -> Result {
    let root = tempfile::tempdir()?;
    let step = AddPartyStep::ProposeClearOnboarding;
    let config = add_party_upload_run(&db, step).await?;
    let server = start(&db, &root, step).await?;
    let proposal = utils::encode_length_prefixed_message(&SignedTopologyTransaction::default());
    send(&server, 1, MessageType::AddPartyClearProposal, &proposal).await?;
    send(&server, 2, MessageType::StatusUpdate, b"skip").await?;
    drop(server);

    let instance = WorkflowInstance::new(
        RUN.into(),
        WorkflowKind::AddParty,
        WorkflowRole::Coordinator,
    );
    let coordinating = tokio::spawn(start_add_party_coordinator(
        NodeConfig::default().with_root_dir(root.path()),
        NetworkConfig::default(),
        config.clone(),
        None,
        db.clone(),
        Arc::new(RwLock::new(HashMap::new())),
        instance.clone(),
    ));
    let command = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let active = instance.active.read().unwrap().clone();
            if let Some(active) = active {
                let command = active
                    .handle_command(peer(2), Message::new_empty(MessageType::GetNextCommand))
                    .await?;
                if command.msg_type == MessageType::SignClearOnboarding {
                    break Ok::<_, NoiseError>(command);
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    coordinating.abort();
    let _ = coordinating.await;
    let decoded = utils::decode_length_prefixed(&command.payload, 2)?;
    assert_eq!(decoded[0], serde_json::to_vec(&config)?);
    assert_eq!(decoded[1], proposal);
    assert_eq!(
        db.read_artifact(RUN, artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL, None)
            .await?,
        Some(proposal)
    );
    Ok(())
}

async fn assert_upload_read_failure(server: &NoiseServer<AddPartyStep>) -> Result {
    let payload = utils::encode_length_prefixed(&[b"keys", b"participant"]);
    let reply = server
        .handle_command(
            peer(1),
            Message::new(MessageType::AddPartyKeysUpload, payload),
        )
        .await?;
    assert_eq!(reply.msg_type, MessageType::Error);
    assert_eq!(reply.payload, b"The coordinator could not store the upload");
    assert!(!server.workflow_state.has_peer_completed(&peer(1)).await);
    assert_eq!(
        server.workflow_state.current_step().await,
        AddPartyStep::GenerateNewMemberKeys
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_missing_upload_run_returns_a_storage_error(db: SqlitePool) -> Result {
    let root = tempfile::tempdir()?;
    let step = AddPartyStep::GenerateNewMemberKeys;
    add_party_upload_run(&db, step).await?;
    let server = start(&db, &root, step).await?;
    sqlx::query("DELETE FROM workflow_runs WHERE instance_name = ?")
        .bind(RUN)
        .execute(&db)
        .await?;
    assert_upload_read_failure(&server).await
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_failed_upload_artifact_read_returns_a_storage_error(db: SqlitePool) -> Result {
    let root = tempfile::tempdir()?;
    let step = AddPartyStep::GenerateNewMemberKeys;
    add_party_upload_run(&db, step).await?;
    let server = start(&db, &root, step).await?;
    sqlx::query("DROP TABLE workflow_artifacts")
        .execute(&db)
        .await?;
    assert_upload_read_failure(&server).await
}
