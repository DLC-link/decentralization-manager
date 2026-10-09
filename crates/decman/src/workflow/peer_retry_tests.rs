//! Drive the real peer loop with scripted Noise commands, without a Canton node.

use std::{collections::VecDeque, sync::Mutex, time::Duration};

use hyper::{Body, Response};
use tokio::net::TcpListener;
use tokio_noise::handshakes::nn_psk2::Responder;

use super::*;
use crate::noise::{Message, NoiseKeypair};

/// What the scripted coordinator saw of one peer run.
struct PeerRun {
    result: Result,
    polls: Vec<std::time::Instant>,
    completions: usize,
    /// Failure reports the peer sent after giving up.
    reports: Vec<DeclineInvitationPayload>,
}

/// Drive the real peer loop through `commands` from a scripted coordinator.
async fn run_commands(
    db: SqlitePool,
    kind: WorkflowKind,
    commands: Vec<Message>,
) -> Result<PeerRun> {
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
    let reports = Arc::new(Mutex::new(Vec::new()));
    let commands = Arc::new(Mutex::new(VecDeque::from(commands)));
    let server_polls = polls.clone();
    let server_completions = completions.clone();
    let server_reports = reports.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await?;
            let commands = commands.clone();
            let polls = server_polls.clone();
            let completions = server_completions.clone();
            let reports = server_reports.clone();
            hyper_noise::server::serve_http(
                socket,
                Responder::new(move |_: &[u8]| Some(psk)),
                move |_: &[u8], request: hyper::Request<Body>| {
                    let commands = commands.clone();
                    let polls = polls.clone();
                    let completions = completions.clone();
                    let reports = reports.clone();
                    async move {
                        let bytes = hyper::body::to_bytes(request.into_body()).await?;
                        let message = Message::from_bytes(&bytes)?;
                        let reply = if message.msg_type == MessageType::DeclineInvitation {
                            reports
                                .lock()
                                .map_err(|_| anyhow::anyhow!("report lock poisoned"))?
                                .push(serde_json::from_slice(&message.payload)?);
                            Message::new_empty(MessageType::Ack)
                        } else if message.msg_type == MessageType::GetNextCommand {
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
    if matches!(outcome, Ok(Err(_))) {
        // The peer reports its failure from a background task.
        let reported = async {
            while reports.lock().map_or(true, |reports| reports.is_empty()) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(5), reported).await;
    }
    server.abort();
    match server.await {
        Err(error) if error.is_cancelled() => {}
        result => result??,
    }
    let polls = polls
        .lock()
        .map_err(|_| anyhow::anyhow!("poll lock poisoned"))?
        .clone();
    let reports = reports
        .lock()
        .map_err(|_| anyhow::anyhow!("report lock poisoned"))?
        .clone();
    Ok(PeerRun {
        result: outcome?,
        polls,
        completions: completions.load(std::sync::atomic::Ordering::Relaxed),
        reports,
    })
}

/// Check that a peer which gave up told the coordinator, once, and why.
fn assert_reported(run: &PeerRun, kind: WorkflowKind) {
    let [report] = run.reports.as_slice() else {
        panic!("expected one failure report, got {}", run.reports.len());
    };
    let error = run.result.as_ref().err().map(|e| format!("{e:#}"));
    assert!(report.abandoned, "the report was sent as a plain decline");
    assert_eq!(report.kind, kind);
    assert_eq!(report.workflow_instance.as_deref(), Some("coordinator-run"));
    assert_eq!(
        report.reason, error,
        "the report must carry the peer's error"
    );
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
            let run = run_commands(db, kind, vec![message; MAX_CONSECUTIVE_STEP_FAILURES]).await?;
            let error = run
                .result
                .as_ref()
                .err()
                .ok_or_else(|| anyhow::anyhow!("{command:?} did not abort"))?;
            assert!(
                error.to_string().contains(&format!("({command:?})")),
                "{error}"
            );
            assert_reported(&run, kind);
            assert_eq!(
                run.polls.len(),
                MAX_CONSECUTIVE_STEP_FAILURES,
                "{command:?}"
            );
            assert_eq!(run.completions, 0, "malformed {command:?} was completed");
            for pair in run.polls.windows(2) {
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
    let (control, recovered) = tokio::try_join!(
        run_commands(db.clone(), WorkflowKind::AddParty, controls),
        run_commands(db, WorkflowKind::AddParty, recovered),
    )?;
    assert!(control.result.is_err(), "Wait/Ping reset the failure count");
    assert_reported(&control, WorkflowKind::AddParty);
    assert_eq!(
        control.polls.len(),
        (MAX_CONSECUTIVE_STEP_FAILURES - 1) * 3 + 1
    );
    recovered.result?;
    assert_eq!(recovered.polls.len(), MAX_CONSECUTIVE_STEP_FAILURES + 2);
    assert_eq!(recovered.completions, 1);
    assert!(
        recovered.reports.is_empty(),
        "a peer that finished reported a failure"
    );
    Ok(())
}
