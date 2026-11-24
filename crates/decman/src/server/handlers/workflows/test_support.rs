//! Runs, party ids, app states, cached parties, kick fixtures, an admin
//! request, a test node and a log capture shared by the workflow handler
//! tests.

use std::{
    collections::HashSet,
    io::{self, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
};

use actix_web::{
    App, HttpMessage,
    dev::{HttpServiceFactory, ServiceResponse},
    http::StatusCode,
    test, web,
};
use canton_proto_rs::com::digitalasset::canton::crypto::v30::{SigningKeyUsage, SigningPublicKey};
use sqlx::SqlitePool;

use crate::{
    auth::Principal,
    canton_id::CantonId,
    config::Peer,
    db::{
        rows::{DecPartyParticipantRow, DecPartyRow},
        schema::{Commitable, SchemaWrite},
    },
    error::Result,
    noise::NoiseKeypair,
    server::{
        AppState, ConnectionStatus,
        health::HealthReply,
        types::{ErrorResponse, WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun},
    },
    utils,
};

/// A minimal coordinator run row for `party`, in progress.
pub fn in_progress_run(instance: &str, party: &CantonId) -> WorkflowRun {
    WorkflowRun {
        instance_name: instance.to_string(),
        kind: WorkflowKind::Contracts,
        role: WorkflowRole::Coordinator,
        status: WorkflowProgress::InProgress,
        current_step: "SubmitProposals".to_string(),
        step_index: 0,
        step_total: 1,
        config_json: "{}".to_string(),
        coordinator_pubkey: None,
        coordinator_instance: None,
        coordinator_name: None,
        expected_peers: Vec::new(),
        completed_peers: Vec::new(),
        connected_peers: Vec::new(),
        acs_progress: None,
        dec_party_id: Some(party.clone()),
        prefix: None,
        participants: Vec::new(),
        previous_threshold: None,
        new_threshold: None,
        kicked_participant: None,
        kicked_owner_key: None,
        added_participant: None,
        package_names: Vec::new(),
        dar_filenames: Vec::new(),
        error: None,
        dismissed: false,
        created_at: 0,
        updated_at: 0,
    }
}

pub fn pid(tag: u8) -> anyhow::Result<CantonId> {
    let ns = format!("1220{:0>64}", format!("{tag:02x}"));
    CantonId::parse(&format!("validator-{tag}::{ns}"))
}

pub fn host_row(uid: &CantonId, owner_key: Option<&str>) -> DecPartyParticipantRow {
    DecPartyParticipantRow {
        dec_party_id: "party".to_string(),
        participant_uid: uid.to_string(),
        permission: "confirmation".to_string(),
        owner_key: owner_key.map(str::to_string),
        signing_key: None,
    }
}

pub fn health_reply(version: &str, features: &[&str]) -> HealthReply {
    HealthReply {
        status: ConnectionStatus::Connected,
        workflow: None,
        version: Some(version.to_string()),
        build_version: None,
        features: features.iter().map(ToString::to_string).collect(),
    }
}

pub fn ids(tags: &[u8]) -> anyhow::Result<HashSet<CantonId>> {
    tags.iter().map(|tag| pid(*tag)).collect()
}

/// Caches `party` at namespace threshold `threshold`, hosted by `hosts`
/// (this node is `pid(1)`), each under its own owner key `k{tag}`. The
/// keys in `owners_only` own the party without hosting it. Only `peers`
/// are configured peers of this node.
///
/// The party also gets a run in flight. A kick that passes every check
/// then stops at the in-flight guard with a 409, before it probes or
/// invites any peer.
pub async fn cache_kick_party(
    pool: &SqlitePool,
    party: &CantonId,
    threshold: i64,
    hosts: &[u8],
    owners_only: &[&str],
    peers: &[u8],
) -> Result {
    cache_party(pool, party, threshold, None).await?;
    let mut keys: Vec<String> = hosts.iter().map(|tag| format!("k{tag}")).collect();
    let host_rows = hosts
        .iter()
        .zip(&keys)
        .map(|(tag, key)| Ok(host_row(&pid(*tag)?, Some(key))))
        .collect::<anyhow::Result<Vec<_>>>()?;
    keys.extend(owners_only.iter().map(ToString::to_string));
    let mut tx = pool.begin_transaction().await?;
    tx.replace_dec_party_participants(party, &host_rows).await?;
    tx.replace_dec_party_owners(party, &keys).await?;
    for tag in peers {
        tx.insert_peer(&Peer {
            participant_id: pid(*tag)?,
            name: format!("peer-{tag}"),
            address: "127.0.0.1".to_string(),
            port: 1,
            public_key: String::new(),
            party: None,
        })
        .await?;
    }
    Commitable::commit(tx).await?;
    insert_run(pool, &in_progress_run("contracts-in-flight", party)).await
}

pub fn kick_public_key(tag: u8) -> SigningPublicKey {
    SigningPublicKey {
        public_key: vec![tag; 32],
        usage: vec![SigningKeyUsage::Protocol as i32],
        ..Default::default()
    }
}

/// Records `kick_public_key(key)` as the signing key of host `pid(host)`.
pub async fn record_signing_keys(pool: &SqlitePool, party: &CantonId, keys: &[(u8, u8)]) -> Result {
    for (host, key) in keys {
        sqlx::query(
            "UPDATE dec_party_participant SET signing_key = ? \
             WHERE dec_party_id = ? AND participant_uid = ?",
        )
        .bind(utils::compute_fingerprint(&kick_public_key(*key)).to_string())
        .bind(party.to_string())
        .bind(pid(*host)?.to_string())
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn cache_party(
    pool: &SqlitePool,
    party: &CantonId,
    threshold: i64,
    confirmation_threshold: Option<i64>,
) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&DecPartyRow {
        party_id: party.to_string(),
        prefix: party.prefix.clone(),
        threshold,
        updated_at: 0,
        my_owner_key: None,
        confirmation_threshold,
    })
    .await?;
    Commitable::commit(tx).await
}

pub fn test_cid(prefix: &str) -> anyhow::Result<CantonId> {
    let ns = format!("1220{:0>64}", "a");
    CantonId::parse(&format!("{prefix}::{ns}"))
}

pub async fn insert_run(pool: &SqlitePool, run: &WorkflowRun) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(run).await?;
    Commitable::commit(tx).await
}

/// `AppState::for_test` on a migrated database.
pub async fn state_on(pool: SqlitePool) -> Result<web::Data<AppState>> {
    let state = Arc::try_unwrap(AppState::for_test(None).await?.into_inner())
        .map_err(|_| anyhow::anyhow!("a fresh test state has one owner"))?;
    Ok(web::Data::new(AppState { db: pool, ..state }))
}

pub fn admin() -> Principal {
    Principal {
        sub: "admin".to_string(),
        issuer: "test".to_string(),
        roles: Vec::new(),
        email: None,
    }
}

pub async fn status_and_body(response: ServiceResponse) -> (StatusCode, String) {
    let status = response.status();
    let body = test::read_body(response).await;
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// A node under test: its state on a test database, with a participant id and
/// a Noise key in a temporary root directory.
pub struct TestNode {
    pub data: web::Data<AppState>,
    _root: tempfile::TempDir,
}

/// A node whose participant id is `self_id`, on `pool`. Its Canton ports
/// have nothing listening on them, so every Canton call fails.
pub async fn test_node(pool: SqlitePool, self_id: &CantonId) -> anyhow::Result<TestNode> {
    let root = tempfile::tempdir()?;
    let mut state = AppState::for_test_on(None, pool).into_inner();
    let node = Arc::get_mut(&mut state)
        .ok_or_else(|| anyhow::anyhow!("a fresh test state has one owner"))?;
    node.config = node.config.clone().with_root_dir(root.path());
    node.config.node.participant_id = Some(self_id.clone());
    node.config.canton.admin_api_port = closed_port()?;
    node.config.canton.ledger_api_port = closed_port()?;
    tokio::fs::create_dir_all(node.config.data_dir()).await?;
    NoiseKeypair::generate()
        .save_to_file(node.config.key_file_path())
        .await?;
    Ok(TestNode {
        data: web::Data::from(state),
        _root: root,
    })
}

/// A port with nothing listening on it.
pub fn closed_port() -> anyhow::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// A peer on a port where nothing listens. With `reachable_key` its Noise
/// key parses, so a health probe tries it and fails. Without it the key is
/// empty, so the probe and the invites skip the peer.
pub fn peer(id: &CantonId, port: u16, reachable_key: bool) -> Peer {
    Peer {
        participant_id: id.clone(),
        name: id.prefix.clone(),
        address: "127.0.0.1".to_string(),
        port,
        public_key: if reachable_key {
            NoiseKeypair::generate().public_key_hex()
        } else {
            String::new()
        },
        party: None,
    }
}

pub async fn insert_peers(pool: &SqlitePool, peers: &[Peer]) -> anyhow::Result<()> {
    let mut tx = pool.begin_transaction().await?;
    for peer in peers {
        tx.insert_peer(peer).await?;
    }
    Commitable::commit(tx).await
}

/// Cache `party` with namespace threshold `threshold`, its hosts (each with
/// its owner key, if known) and its owner keys (each with its claimant, if
/// any).
pub async fn cache_hosted_party(
    pool: &SqlitePool,
    party: &CantonId,
    threshold: i64,
    hosts: &[(&CantonId, Option<&str>)],
    owners: &[(&str, Option<&CantonId>)],
) -> anyhow::Result<()> {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&DecPartyRow {
        party_id: party.to_string(),
        prefix: party.prefix.clone(),
        threshold,
        updated_at: 0,
        my_owner_key: None,
        confirmation_threshold: None,
    })
    .await?;
    let rows: Vec<DecPartyParticipantRow> = hosts
        .iter()
        .map(|(uid, owner_key)| DecPartyParticipantRow {
            dec_party_id: party.to_string(),
            participant_uid: uid.to_string(),
            permission: "confirmation".to_string(),
            owner_key: owner_key.map(str::to_string),
            signing_key: None,
        })
        .collect();
    tx.replace_dec_party_participants(party, &rows).await?;
    let keys: Vec<String> = owners.iter().map(|(key, _)| key.to_string()).collect();
    tx.replace_dec_party_owners(party, &keys).await?;
    for (key, claimant) in owners {
        if let Some(claimant) = claimant {
            tx.update_owner_claim(party, key, &claimant.to_string())
                .await?;
        }
    }
    Commitable::commit(tx).await
}

/// Make every read of `table` fail, so a handler meets a database error.
pub async fn break_table(pool: &SqlitePool, table: &str) -> anyhow::Result<()> {
    sqlx::query(&format!("ALTER TABLE {table} RENAME TO {table}_gone"))
        .execute(pool)
        .await?;
    Ok(())
}

/// Make every new run row fail to write while reads still work, so a start
/// passes its party claim and meets a database error at its run write.
pub async fn refuse_run_writes(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TRIGGER refuse_run_writes BEFORE INSERT ON workflow_runs \
         BEGIN SELECT RAISE(ABORT, 'workflow_runs refuses writes'); END",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// What a handler answered.
#[derive(Debug)]
pub struct Answer {
    pub status: StatusCode,
    pub content_type: Option<String>,
    /// Every header as `(name, value)`, sorted.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Answer {
    /// Assert a JSON `ErrorResponse` with `status` whose text is exactly
    /// `error`, and no header but its content type.
    #[track_caller]
    pub fn assert_error(&self, status: StatusCode, error: &str) {
        assert_eq!(self.status, status, "{}", self.body);
        assert_eq!(self.content_type.as_deref(), Some("application/json"));
        assert_eq!(
            self.headers,
            vec![("content-type".to_string(), "application/json".to_string())]
        );
        let expected = serde_json::to_string(&ErrorResponse {
            error: error.to_string(),
        })
        .expect("an ErrorResponse serializes");
        assert_eq!(self.body, expected);
    }

    /// The body as JSON.
    pub fn json(&self) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::from_str(&self.body)?)
    }
}

/// POST `body` to `uri` on an app that serves only `service`, as `principal`
/// (no principal means an anonymous caller).
pub async fn post_as(
    data: &web::Data<AppState>,
    service: impl HttpServiceFactory + 'static,
    uri: &str,
    body: serde_json::Value,
    principal: Option<Principal>,
) -> Answer {
    let app = test::init_service(App::new().app_data(data.clone()).service(service)).await;
    let request = test::TestRequest::post()
        .uri(uri)
        .set_json(body)
        .to_request();
    if let Some(principal) = principal {
        request.extensions_mut().insert(principal);
    }
    answer_of(test::call_service(&app, request).await).await
}

/// Read what a handler answered.
pub async fn answer_of(response: ServiceResponse) -> Answer {
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    headers.sort();
    let body = String::from_utf8_lossy(&test::read_body(response).await).into_owned();
    Answer {
        status,
        content_type,
        headers,
        body,
    }
}

/// POST `body` to `uri` as an admin.
pub async fn post_as_admin(
    data: &web::Data<AppState>,
    service: impl HttpServiceFactory + 'static,
    uri: &str,
    body: serde_json::Value,
) -> Answer {
    post_as(data, service, uri, body, Some(admin())).await
}

/// The log lines written while [`CapturedLogs::subscriber`] is the thread's
/// default subscriber. Each line reads `LEVEL target: message`.
#[derive(Clone, Default)]
pub struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("the log buffer lock is poisoned"))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl CapturedLogs {
    /// A subscriber that writes every level into this capture, without
    /// timestamps or colours.
    pub fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + 'static {
        let logs = self.clone();
        tracing_subscriber::fmt()
            .with_writer(move || logs.clone())
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::TRACE)
            .finish()
    }

    /// The captured lines, oldest first.
    pub fn lines(&self) -> anyhow::Result<Vec<String>> {
        let bytes = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("the log buffer lock is poisoned"))?;
        Ok(String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// The captured lines whose tracing target is exactly `target`, oldest
    /// first.
    pub fn lines_at(&self, target: &str) -> anyhow::Result<Vec<String>> {
        let prefix = format!("{target}: ");
        Ok(self
            .lines()?
            .into_iter()
            .filter(|line| line.get(6..).is_some_and(|rest| rest.starts_with(&prefix)))
            .collect())
    }
}

/// Assert that `lines` match `expected` one for one. An expected line that
/// ends in `": "` is a prefix, because an error text follows it; any other
/// expected line must match exactly.
#[track_caller]
pub fn assert_lines(lines: &[String], expected: &[String]) {
    assert_eq!(lines.len(), expected.len(), "{lines:#?}");
    for (line, expected) in lines.iter().zip(expected) {
        if expected.ends_with(": ") {
            assert!(
                line.starts_with(expected.as_str()),
                "{line:?} != {expected:?}..."
            );
        } else {
            assert_eq!(line, expected);
        }
    }
}
