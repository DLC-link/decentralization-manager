//! Drive the real peer loop with scripted Noise commands, without a Canton node.

use std::{collections::VecDeque, sync::Mutex, time::Duration};

use hyper::{Body, Response};
use tokio::net::TcpListener;
use tokio_noise::handshakes::nn_psk2::Responder;

use super::*;
use crate::noise::{Message, NoiseKeypair};

/// Return the peer's outcome, poll times, and number of completion messages.
async fn run_commands(
    db: SqlitePool,
    kind: WorkflowKind,
    commands: Vec<Message>,
) -> Result<(Result, Vec<std::time::Instant>, usize)> {
    let root = tempfile::tempdir()?;
    let mut node = NodeConfig::default().with_root_dir(root.path());
    node.node.participant_id = Some(CantonId::parse(&format!("peer::1220{}", "a".repeat(64)))?);
    let keys = NoiseKeypair::generate();
    tokio::fs::create_dir_all(node.data_dir()).await?;
    keys.save_to_file(node.key_file_path()).await?;

    let coordinator_keys = NoiseKeypair::generate();
    let psk = *coordinator_keys.derive_psk(&keys.public_key);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let coordinator = Peer {
        participant_id: CantonId::parse(&format!("coordinator::1220{}", "b".repeat(64)))?,
        name: "Scripted coordinator".into(),
        address: "127.0.0.1".into(),
        port: listener.local_addr()?.port(),
        public_key: coordinator_keys.public_key_hex(),
        party: None,
    };
    let instance = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO workflow_runs
         (instance_name, kind, role, status, current_step, step_index, step_total,
          config_json, expected_peers_json, completed_peers_json, created_at, updated_at)
         VALUES (?, ?, 'Peer', 'inprogress', 'Active', 0, 1, '{}', '[]', '[]', 0, 0)",
    )
    .bind(&instance)
    .bind(kind.as_str())
    .execute(&db)
    .await?;

    let polls = Arc::new(Mutex::new(Vec::new()));
    let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let commands = Arc::new(Mutex::new(VecDeque::from(commands)));
    let server_polls = polls.clone();
    let server_completions = completions.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await?;
            let commands = commands.clone();
            let polls = server_polls.clone();
            let completions = server_completions.clone();
            hyper_noise::server::serve_http(
                socket,
                Responder::new(move |_: &[u8]| Some(psk)),
                move |_: &[u8], request: hyper::Request<Body>| {
                    let commands = commands.clone();
                    let polls = polls.clone();
                    let completions = completions.clone();
                    async move {
                        let bytes = hyper::body::to_bytes(request.into_body()).await?;
                        let message = Message::from_bytes(&bytes)?;
                        let reply = if message.msg_type == MessageType::GetNextCommand {
                            polls
                                .lock()
                                .map_err(|_| anyhow::anyhow!("poll lock poisoned"))?
                                .push(std::time::Instant::now());
                            commands
                                .lock()
                                .map_err(|_| anyhow::anyhow!("command lock poisoned"))?
                                .pop_front()
                                .unwrap_or_else(|| Message::new_empty(MessageType::Disconnect))
                        } else {
                            completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            Message::new_empty(MessageType::Wait)
                        };
                        Ok::<_, anyhow::Error>(Response::new(Body::from(reply.to_bytes())))
                    }
                },
                Some(Duration::from_secs(5)),
            )
            .await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        start_peer(
            node,
            coordinator,
            db,
            instance,
            "coordinator-run".into(),
            None,
        ),
    )
    .await;
    server.abort();
    match server.await {
        Err(error) if error.is_cancelled() => {}
        result => result??,
    }
    let poll_times = polls
        .lock()
        .map_err(|_| anyhow::anyhow!("poll lock poisoned"))?
        .clone();
    Ok((
        outcome?,
        poll_times,
        completions.load(std::sync::atomic::Ordering::Relaxed),
    ))
}

#[sqlx::test(migrations = "./migrations")]
async fn malformed_commands_back_off_and_exhaust_the_budget(db: SqlitePool) -> Result {
    let mut cases = Vec::new();
    for (kind, command, item_count) in [
        (WorkflowKind::Contracts, MessageType::SignSubmissions, 2),
        (WorkflowKind::Kick, MessageType::SignKick, 3),
        (
            WorkflowKind::ChangeThreshold,
            MessageType::SignChangeThreshold,
            3,
        ),
        (WorkflowKind::AddParty, MessageType::SignAddParty, 3),
        (WorkflowKind::AddParty, MessageType::ImportAcs, 2),
    ] {
        for payload in [
            Vec::new(),
            vec![0xff],
            utils::encode_length_prefixed(&vec![b"{}".as_slice(); item_count]),
        ] {
            cases.push((kind, Message::new(command, payload)));
        }
    }
    for command in [
        MessageType::GenerateAddPartyKeys,
        MessageType::ClearOnboardingFlag,
    ] {
        cases.push((
            WorkflowKind::AddParty,
            Message::new(command, b"{}".to_vec()),
        ));
    }
    cases.push((
        WorkflowKind::AddParty,
        Message::new_empty(MessageType::SignClearOnboarding),
    ));
    cases.push((
        WorkflowKind::Onboarding,
        Message::new_empty(MessageType::GenerateKeys),
    ));

    futures::future::try_join_all(cases.into_iter().map(|(kind, message)| {
        let db = db.clone();
        async move {
            let command = message.msg_type;
            let (result, polls, completions) =
                run_commands(db, kind, vec![message; MAX_CONSECUTIVE_STEP_FAILURES]).await?;
            let error = result
                .err()
                .ok_or_else(|| anyhow::anyhow!("{command:?} did not abort"))?;
            assert!(
                error.to_string().contains(&format!("({command:?})")),
                "{error}"
            );
            assert_eq!(polls.len(), MAX_CONSECUTIVE_STEP_FAILURES, "{command:?}");
            assert_eq!(completions, 0, "malformed {command:?} was completed");
            for pair in polls.windows(2) {
                assert!(
                    pair[1].duration_since(pair[0]) >= Duration::from_secs(2),
                    "{command:?} polled without backoff"
                );
            }
            Ok::<(), anyhow::Error>(())
        }
    }))
    .await?;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn control_messages_preserve_failures_but_completed_steps_reset_them(
    db: SqlitePool,
) -> Result {
    let bad = Message::new_empty(MessageType::SignClearOnboarding);
    let mut controls = Vec::new();
    for _ in 0..MAX_CONSECUTIVE_STEP_FAILURES {
        controls.push(bad.clone());
        controls.push(Message::new_empty(MessageType::Wait));
        controls.push(Message::new_empty(MessageType::Ping));
    }
    let mut recovered = vec![bad.clone(); MAX_CONSECUTIVE_STEP_FAILURES - 1];
    // Empty proposal is the coordinator's legitimate "already cleared" marker.
    recovered.push(Message::new(
        MessageType::SignClearOnboarding,
        utils::encode_length_prefixed(&[b"{}", b""]),
    ));
    recovered.push(bad);
    let (control_result, recovered_result) = tokio::try_join!(
        run_commands(db.clone(), WorkflowKind::AddParty, controls),
        run_commands(db, WorkflowKind::AddParty, recovered),
    )?;
    assert!(
        control_result.0.is_err(),
        "Wait/Ping reset the failure count"
    );
    assert_eq!(
        control_result.1.len(),
        (MAX_CONSECUTIVE_STEP_FAILURES - 1) * 3 + 1
    );
    recovered_result.0?;
    assert_eq!(recovered_result.1.len(), MAX_CONSECUTIVE_STEP_FAILURES + 2);
    assert_eq!(recovered_result.2, 1);
    Ok(())
}
