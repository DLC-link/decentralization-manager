use super::*;
use crate::{
    canton_id::{NAMESPACE_LENGTH, Namespace},
    db::MIGRATOR,
    error::Result,
    workflow::{kick::coordinator::save_signature_pairs, storage::WorkflowStorage},
};

const RUN: &str = "signature-run";
const PAIR: &[u8] = &[2, 11, 12, 3, 21, 22, 23];

fn peer(n: u8) -> CantonId {
    CantonId::new(format!("p{n}"), Namespace::new([n; NAMESPACE_LENGTH]))
}

async fn insert_run<S: WorkflowStep>(db: &SqlitePool, step: S) -> Result {
    sqlx::query(
        "INSERT INTO workflow_runs
         (instance_name, kind, role, status, current_step, step_index, step_total,
          config_json, expected_peers_json, completed_peers_json, created_at, updated_at)
         VALUES (?, ?, 'Coordinator', 'inprogress', ?, ?, ?, '{}', ?, '[]', 0, 0)",
    )
    .bind(RUN)
    .bind(S::kind().as_str())
    .bind(step.step_name())
    .bind(step.step_index())
    .bind(S::step_total())
    .bind(serde_json::to_string(&[peer(1), peer(2)])?)
    .execute(db)
    .await?;
    Ok(())
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
