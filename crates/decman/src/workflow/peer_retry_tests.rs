//! Drive the real peer loop with scripted Noise commands, without a Canton node.

use std::{
    collections::VecDeque,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    UploadDarRequest, UploadDarResponse,
};
use hyper::{Body, Response, header::HeaderValue};
use prost::Message as _;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_noise::handshakes::nn_psk2::Responder;

use super::*;
use crate::{
    noise::{CHUNK_SIZE, MAX_PAYLOAD_SIZE, Message, NoiseKeypair},
    server::RequestDarPayload,
};

/// The coordinator's run instance, as the accepted invitation names it.
pub(super) const COORDINATOR_RUN: &str = "coordinator-run";

/// What the scripted coordinator saw of one peer run.
pub(super) struct PeerRun {
    pub(super) result: Result,
    pub(super) polls: Vec<Instant>,
    pub(super) completions: usize,
    /// Failure reports the peer sent after giving up.
    pub(super) reports: Vec<DeclineInvitationPayload>,
    /// The DARs the peer asked for with `RequestDar`, in order.
    pub(super) dar_requests: Vec<RequestDarPayload>,
    /// Chunks the peer read of a chunked command's payload.
    pub(super) command_chunks: usize,
    /// The DARs the peer uploaded to its participant, as (description, bytes).
    pub(super) uploads: Vec<(String, Vec<u8>)>,
}

/// A scripted coordinator's side of one peer run.
#[derive(Default)]
pub(super) struct Script {
    /// Answers to the peer's polls, in order. `Disconnect` once they run out.
    pub(super) commands: Vec<Message>,
    /// The DAR filenames of the accepted invitation.
    pub(super) dar_filenames: Vec<String>,
    /// The DAR hashes of the accepted invitation.
    pub(super) dar_hashes: Vec<String>,
    /// The payload of a chunked command, read by a `GetChunk` routed to the run.
    pub(super) command_payload: Vec<u8>,
    /// The run's DARs, served by index to `RequestDar`.
    pub(super) dars: Vec<(String, Vec<u8>)>,
}

/// The scripted coordinator, shared by every connection the peer opens.
#[derive(Default)]
struct Coordinator {
    commands: Mutex<VecDeque<Message>>,
    command_payload: Vec<u8>,
    dars: Vec<(String, Vec<u8>)>,
    /// The DAR whose chunks an unrouted `GetChunk` reads.
    dar_chunks: Mutex<Vec<u8>>,
    polls: Mutex<Vec<Instant>>,
    completions: AtomicUsize,
    reports: Mutex<Vec<DeclineInvitationPayload>>,
    dar_requests: Mutex<Vec<RequestDarPayload>>,
    command_chunks: AtomicUsize,
}

impl Coordinator {
    fn answer(&self, message: Message) -> Result<Message> {
        Ok(match message.msg_type {
            MessageType::DeclineInvitation => {
                lock(&self.reports)?.push(serde_json::from_slice(&message.payload)?);
                Message::new_empty(MessageType::Ack)
            }
            MessageType::GetNextCommand => {
                lock(&self.polls)?.push(Instant::now());
                lock(&self.commands)?
                    .pop_front()
                    .unwrap_or_else(|| Message::new_empty(MessageType::Disconnect))
            }
            MessageType::RequestDar => self.serve_dar(&message.payload)?,
            // An unrouted chunk belongs to a DAR, a routed one to the command.
            MessageType::GetChunk if message.instance.is_empty() => {
                chunk(&lock(&self.dar_chunks)?, &message.payload)?
            }
            MessageType::GetChunk => {
                self.command_chunks.fetch_add(1, Ordering::Relaxed);
                chunk(&self.command_payload, &message.payload)?
            }
            _ => {
                self.completions.fetch_add(1, Ordering::Relaxed);
                Message::new_empty(MessageType::Wait)
            }
        })
    }

    /// Serve DAR `index` as the coordinator does: inline while it fits one
    /// frame, chunked above that.
    fn serve_dar(&self, payload: &[u8]) -> Result<Message> {
        let request: RequestDarPayload = serde_json::from_slice(payload)?;
        lock(&self.dar_requests)?.push(request.clone());
        let Some((filename, dar)) = self.dars.get(request.index) else {
            let refusal = format!("run has no DAR at index {}", request.index);
            return Ok(Message::new(MessageType::Error, refusal.into_bytes()));
        };
        if request.workflow_instance != COORDINATOR_RUN || *filename != request.filename {
            let refusal = format!("DAR {} of this run is {filename}", request.index);
            return Ok(Message::new(MessageType::Error, refusal.into_bytes()));
        }
        if dar.len() <= MAX_PAYLOAD_SIZE {
            return Ok(Message::new(MessageType::Data, dar.clone()));
        }
        *lock(&self.dar_chunks)? = dar.clone();
        Ok(Message::new(
            MessageType::ChunkedCommand,
            chunked_meta(MessageType::Data, dar.len())?,
        ))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| anyhow::anyhow!("scripted coordinator lock poisoned"))
}

/// `ChunkedCommand` metadata that announces `size` bytes of `command`.
pub(super) fn chunked_meta(command: MessageType, size: usize) -> Result<Vec<u8>> {
    let mut meta = command.to_u16().to_be_bytes().to_vec();
    meta.extend_from_slice(&u32::try_from(size)?.to_be_bytes());
    meta.extend_from_slice(&u32::try_from(size.div_ceil(CHUNK_SIZE))?.to_be_bytes());
    Ok(meta)
}

/// The chunk of `payload` that a `GetChunk` asks for, framed with its index.
fn chunk(payload: &[u8], request: &[u8]) -> Result<Message> {
    let index: [u8; 4] = request.get(..4).context("short GetChunk")?.try_into()?;
    let start = usize::try_from(u32::from_be_bytes(index))? * CHUNK_SIZE;
    let data = payload
        .get(start..payload.len().min(start + CHUNK_SIZE))
        .context("GetChunk past the payload")?;
    let mut framed = index.to_vec();
    framed.extend_from_slice(data);
    Ok(Message::new(MessageType::Chunk, framed))
}

/// The DARs a participant received, as (description, bytes).
type Uploads = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// A participant admin API that accepts every DAR upload and records it.
struct Participant {
    port: u16,
    uploads: Uploads,
    server: JoinHandle<()>,
}

impl Participant {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let recorded = uploads.clone();
        let server = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let recorded = recorded.clone();
                let service = hyper::service::service_fn(move |request| {
                    upload_dar(request, recorded.clone())
                });
                tokio::spawn(
                    hyper::server::conn::Http::new()
                        .http2_only(true)
                        .serve_connection(socket, service),
                );
            }
        });
        Ok(Self {
            port,
            uploads,
            server,
        })
    }
}

/// Answer one unary `PackageService/UploadDar` call with success.
async fn upload_dar(request: hyper::Request<Body>, uploads: Uploads) -> Result<Response<Body>> {
    // A unary gRPC body is one frame: a compression flag, a length, the message.
    let body = hyper::body::to_bytes(request.into_body()).await?;
    let upload = UploadDarRequest::decode(body.get(5..).unwrap_or_default())?;
    for dar in upload.dars {
        lock(&uploads)?.push((dar.description.unwrap_or_default(), dar.bytes));
    }
    let reply = UploadDarResponse::default().encode_to_vec();
    let mut frame = vec![0];
    frame.extend_from_slice(&u32::try_from(reply.len())?.to_be_bytes());
    frame.extend_from_slice(&reply);
    let (mut sender, body) = Body::channel();
    tokio::spawn(async move {
        if sender.send_data(frame.into()).await.is_ok() {
            let mut trailers = hyper::HeaderMap::new();
            trailers.insert("grpc-status", HeaderValue::from_static("0"));
            sender.send_trailers(trailers).await.ok();
        }
    });
    Ok(Response::builder()
        .header(hyper::header::CONTENT_TYPE, "application/grpc")
        .body(body)?)
}

/// Drive the real peer loop through `commands` from a scripted coordinator.
async fn run_commands(kind: WorkflowKind, commands: Vec<Message>) -> Result<PeerRun> {
    run_script(
        kind,
        Script {
            commands,
            ..Script::default()
        },
    )
    .await
}

/// Drive the real peer loop through `script`, with a participant that accepts
/// every upload.
///
/// Each run opens its own database, so concurrent runs never wait on one
/// SQLite write lock.
pub(super) async fn run_script(kind: WorkflowKind, script: Script) -> Result<PeerRun> {
    let root = tempfile::tempdir()?;
    let participant = Participant::start().await?;
    let mut node = NodeConfig::default().with_root_dir(root.path());
    node.node.participant_id = Some(CantonId::parse(&format!("peer::1220{}", "a".repeat(64)))?);
    node.canton.admin_api_host = "127.0.0.1".into();
    node.canton.admin_api_port = participant.port;
    let db = crate::db::connect(&node.db_path()).await?;
    crate::db::MIGRATOR.run(&db).await?;
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
    let invitation = serde_json::json!({
        "dar_filenames": script.dar_filenames,
        "dar_hashes": script.dar_hashes,
    });
    sqlx::query(
        "INSERT INTO workflow_runs
         (instance_name, kind, role, status, current_step, step_index, step_total,
          config_json, expected_peers_json, completed_peers_json, created_at, updated_at)
         VALUES (?, ?, 'Peer', 'inprogress', 'Active', 0, 1, ?, '[]', '[]', 0, 0)",
    )
    .bind(&instance)
    .bind(kind.as_str())
    .bind(invitation.to_string())
    .execute(&db)
    .await?;

    let state = Arc::new(Coordinator {
        commands: Mutex::new(script.commands.into()),
        command_payload: script.command_payload,
        dars: script.dars,
        ..Coordinator::default()
    });
    let served = state.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await?;
            let served = served.clone();
            hyper_noise::server::serve_http(
                socket,
                Responder::new(move |_: &[u8]| Some(psk)),
                move |_: &[u8], request: hyper::Request<Body>| {
                    let served = served.clone();
                    async move {
                        let bytes = hyper::body::to_bytes(request.into_body()).await?;
                        let reply = served.answer(Message::from_bytes(&bytes)?)?;
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
            db.clone(),
            instance,
            COORDINATOR_RUN.into(),
            None,
        ),
    )
    .await;
    if matches!(outcome, Ok(Err(_))) {
        // The peer reports its failure from a background task.
        let reported = async {
            while lock(&state.reports).map_or(true, |reports| reports.is_empty()) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(5), reported).await;
    }
    server.abort();
    participant.server.abort();
    match server.await {
        Err(error) if error.is_cancelled() => {}
        result => result??,
    }
    db.close().await;
    let uploads = lock(&participant.uploads)?.clone();
    Ok(PeerRun {
        result: outcome?,
        polls: lock(&state.polls)?.clone(),
        completions: state.completions.load(Ordering::Relaxed),
        reports: lock(&state.reports)?.clone(),
        dar_requests: lock(&state.dar_requests)?.clone(),
        command_chunks: state.command_chunks.load(Ordering::Relaxed),
        uploads,
    })
}

/// Check that a peer which gave up told the coordinator, once, and why.
pub(super) fn assert_reported(run: &PeerRun, kind: WorkflowKind) {
    let [report] = run.reports.as_slice() else {
        panic!("expected one failure report, got {}", run.reports.len());
    };
    let error = run.result.as_ref().err().map(|e| format!("{e:#}"));
    assert!(report.abandoned, "the report was sent as a plain decline");
    assert_eq!(report.kind, kind);
    assert_eq!(report.workflow_instance.as_deref(), Some(COORDINATOR_RUN));
    assert_eq!(
        report.reason, error,
        "the report must carry the peer's error"
    );
}

#[tokio::test]
async fn malformed_commands_back_off_and_exhaust_the_budget() -> Result {
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

    futures::future::try_join_all(cases.into_iter().map(|(kind, message)| async move {
        let command = message.msg_type;
        let run = run_commands(kind, vec![message; MAX_CONSECUTIVE_STEP_FAILURES]).await?;
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
    }))
    .await?;
    Ok(())
}

#[tokio::test]
async fn control_messages_preserve_failures_but_completed_steps_reset_them() -> Result {
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
        run_commands(WorkflowKind::AddParty, controls),
        run_commands(WorkflowKind::AddParty, recovered),
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
