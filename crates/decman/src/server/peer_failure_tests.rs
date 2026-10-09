//! A peer that gives up or goes silent fails the coordinator's run instead of
//! parking it (#442). Drives a real coordinator, its Noise listener, and a real
//! peer over localhost, without a Canton node.

use tokio::net::TcpListener;

use super::*;
use crate::{
    config::{NetworkConfig, Peer},
    consts::PEER_SILENCE_LIMIT,
    noise::server::NoiseServer,
    workflow::{DarsConfig, DarsStep, contracts::DarFile},
};

const COORDINATOR_RUN: &str = "dars-coordinator-run";
const PEER_RUN: &str = "dars-peer-run";

/// A node with its own data directory and Noise key.
struct TestNode {
    _root: tempfile::TempDir,
    config: NodeConfig,
    keys: NoiseKeypair,
}

impl TestNode {
    async fn new(name: &str, fill: char) -> Result<Self> {
        let root = tempfile::tempdir()?;
        let mut config = NodeConfig::default().with_root_dir(root.path());
        let id = format!("{name}::1220{}", fill.to_string().repeat(64));
        config.node.participant_id = Some(CantonId::parse(&id)?);
        let keys = NoiseKeypair::generate();
        tokio::fs::create_dir_all(config.data_dir()).await?;
        keys.save_to_file(config.key_file_path()).await?;
        Ok(Self {
            _root: root,
            config,
            keys,
        })
    }

    fn id(&self) -> CantonId {
        self.config.participant_id().clone()
    }

    /// How another node addresses this one.
    fn as_peer(&self, port: u16) -> Peer {
        Peer {
            participant_id: self.id(),
            name: "test node".into(),
            address: "127.0.0.1".into(),
            port,
            public_key: self.keys.public_key_hex(),
            party: None,
        }
    }
}

async fn insert_run(
    db: &SqlitePool,
    instance: &str,
    role: WorkflowRole,
    step: &str,
    expected_peers: &[CantonId],
) -> Result {
    sqlx::query(
        "INSERT INTO workflow_runs
         (instance_name, kind, role, status, current_step, step_index, step_total,
          config_json, expected_peers_json, completed_peers_json, created_at, updated_at)
         VALUES (?, ?, ?, 'inprogress', ?, 0, 3, '{}', ?, '[]', 0, 0)",
    )
    .bind(instance)
    .bind(WorkflowKind::Dars.as_str())
    .bind(role.as_str())
    .bind(step)
    .bind(serde_json::to_string(expected_peers)?)
    .execute(db)
    .await?;
    Ok(())
}

/// Run the coordinator's always-on Noise listener on `listener`.
async fn listen(
    listener: TcpListener,
    coordinator: &TestNode,
    db: SqlitePool,
    workflows: WorkflowRegistry,
    last_seen: LastSeen,
) -> Result<tokio::task::JoinHandle<()>> {
    let keypair = Arc::new(NoiseKeypair::from_file(coordinator.config.key_file_path()).await?);
    let self_id = coordinator.id();
    let triggers = WorkflowTriggers {
        pending_invitations: Arc::new(RwLock::new(Vec::new())),
        config: coordinator.config.clone(),
        peer_chunk_cache: Arc::new(Mutex::new(HashMap::new())),
        db: db.clone(),
        party_credentials: Arc::new(RwLock::new(Vec::new())),
        workflows,
        peer_job_sender: mpsc::unbounded_channel().0,
    };
    Ok(tokio::spawn(async move {
        while let Ok((socket, address)) = listener.accept().await {
            tokio::spawn(handle_incoming_connection(
                socket,
                address,
                keypair.clone(),
                db.clone(),
                self_id.clone(),
                triggers.clone(),
                last_seen.clone(),
            ));
        }
    }))
}

/// The peer refuses the DARs it is sent, exhausts its step budget, and tells
/// the coordinator. The coordinator fails the run, naming the peer and its
/// reason, instead of waiting at `UploadDars` for a completion.
#[sqlx::test(migrations = "./migrations")]
async fn a_peer_that_gives_up_fails_the_coordinator_run(db: SqlitePool) -> Result {
    let coordinator = TestNode::new("coordinator", 'c').await?;
    let peer = TestNode::new("peer", 'b').await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let mut tx = db.begin_transaction().await?;
    tx.insert_peer(&peer.as_peer(1)).await?;
    Commitable::commit(tx).await?;
    insert_run(
        &db,
        COORDINATOR_RUN,
        WorkflowRole::Coordinator,
        "WaitingForPeers",
        &[peer.id()],
    )
    .await?;
    // The accepted invitation names no DARs, so the peer refuses every upload.
    insert_run(&db, PEER_RUN, WorkflowRole::Peer, "WaitingForPeers", &[]).await?;

    let workflows = WorkflowRegistry::new();
    let last_seen: LastSeen = Arc::new(RwLock::new(HashMap::new()));
    let listener = listen(
        listener,
        &coordinator,
        db.clone(),
        workflows.clone(),
        last_seen.clone(),
    )
    .await?;
    let instance = WorkflowInstance::new(
        COORDINATOR_RUN.into(),
        WorkflowKind::Dars,
        WorkflowRole::Coordinator,
    );
    workflows.insert(instance.clone());
    let dars = DarsConfig {
        dar_files: vec![DarFile {
            filename: "app.dar".into(),
            data: STANDARD.encode(b"dar bytes"),
        }],
        instance_name: COORDINATOR_RUN.into(),
        peer_ids: vec![peer.id()],
    };
    let coordinating = tokio::spawn(workflow::start_coordinator(
        coordinator.config.clone(),
        db.clone(),
        WorkflowType::Dars,
        None,
        None,
        None,
        Some(dars),
        None,
        None,
        None,
        last_seen,
        instance,
    ));

    let peer_result = tokio::time::timeout(
        Duration::from_secs(60),
        workflow::start_peer(
            peer.config.clone(),
            coordinator.as_peer(port),
            db.clone(),
            PEER_RUN.into(),
            COORDINATOR_RUN.into(),
            None,
        ),
    )
    .await?;
    let peer_error = peer_result
        .err()
        .ok_or_else(|| anyhow::anyhow!("the peer accepted DARs it never agreed to"))?;
    let coordinator_result = tokio::time::timeout(Duration::from_secs(30), coordinating).await??;
    listener.abort();

    let error = coordinator_result
        .err()
        .ok_or_else(|| anyhow::anyhow!("the coordinator run did not fail"))?
        .to_string();
    assert!(
        error.starts_with(&format!(
            "UploadDars cannot complete: peer {} gave up: ",
            peer.id()
        )),
        "{error}"
    );
    assert!(error.contains(&format!("{peer_error:#}")), "{error}");
    assert!(
        error.contains("the accepted invitation named none"),
        "{error}"
    );
    Ok(())
}

/// A peer counts as gone once nothing has been heard from it for
/// `PEER_SILENCE_LIMIT`, counted from when this process began serving the run
/// or from the last time the peer was heard, whichever is later.
#[sqlx::test(migrations = "./migrations")]
async fn a_peer_silent_past_the_limit_strands_the_run(db: SqlitePool) -> Result {
    let coordinator = TestNode::new("coordinator", 'c').await?;
    let peer = TestNode::new("peer", 'b').await?;
    insert_run(
        &db,
        COORDINATOR_RUN,
        WorkflowRole::Coordinator,
        "UploadDars",
        &[peer.id()],
    )
    .await?;
    let last_seen: LastSeen = Arc::new(RwLock::new(HashMap::new()));
    let before = Instant::now();
    let server = NoiseServer::new(
        coordinator.config.clone(),
        NetworkConfig::from_peers(vec![peer.as_peer(1)]),
        db,
        COORDINATOR_RUN.into(),
        DarsStep::WaitingForPeers,
        None,
        last_seen.clone(),
    )
    .await?;
    let second = Duration::from_secs(1);

    // Never heard from: the clock starts when the run is served.
    assert_eq!(
        server
            .stranded_reason(before + PEER_SILENCE_LIMIT - second)
            .await,
        None
    );
    let reason = server
        .stranded_reason(Instant::now() + PEER_SILENCE_LIMIT)
        .await
        .ok_or_else(|| anyhow::anyhow!("a peer silent past the limit did not strand the run"))?;
    assert!(
        reason.starts_with(&format!(
            "UploadDars cannot complete: peer {} has been unreachable for ",
            peer.id()
        )),
        "{reason}"
    );

    // Hearing from the peer restarts the clock.
    let heard = Instant::now() + PEER_SILENCE_LIMIT;
    peer_status::bump(&mut *last_seen.write().await, peer.id().to_string(), heard);
    assert_eq!(
        server
            .stranded_reason(heard + PEER_SILENCE_LIMIT - second)
            .await,
        None
    );
    assert!(
        server
            .stranded_reason(heard + PEER_SILENCE_LIMIT)
            .await
            .is_some()
    );
    Ok(())
}
