//! Runs, party ids, an admin request and a test node shared by the workflow
//! handler tests.

use std::sync::Arc;

use actix_web::{App, HttpMessage, dev::HttpServiceFactory, http::StatusCode, test, web};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::Peer,
    db::{
        rows::{DecPartyParticipantRow, DecPartyRow},
        schema::{Commitable, SchemaWrite},
    },
    noise::NoiseKeypair,
    server::{
        AppState,
        types::{ErrorResponse, WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun},
    },
};

use crate::auth::Principal;

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

pub fn test_cid(prefix: &str) -> anyhow::Result<CantonId> {
    let ns = format!("1220{:0>64}", "a");
    CantonId::parse(&format!("{prefix}::{ns}"))
}

pub fn admin() -> Principal {
    Principal {
        sub: "admin".to_string(),
        issuer: "test".to_string(),
        roles: Vec::new(),
        email: None,
    }
}

/// A node under test: its state on a test database, with a participant id and
/// a Noise key in a temporary root directory.
pub struct TestNode {
    pub data: web::Data<AppState>,
    _root: tempfile::TempDir,
}

/// A node whose participant id is `self_id`, on `pool`.
pub async fn test_node(pool: SqlitePool, self_id: &CantonId) -> anyhow::Result<TestNode> {
    let root = tempfile::tempdir()?;
    let mut state = AppState::for_test_on(None, pool).into_inner();
    let node = Arc::get_mut(&mut state)
        .ok_or_else(|| anyhow::anyhow!("a fresh test state has one owner"))?;
    node.config = node.config.clone().with_root_dir(root.path());
    node.config.node.participant_id = Some(self_id.clone());
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
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
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
pub async fn cache_party(
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

/// Record `run` as an in-progress coordinator run.
pub async fn insert_run(pool: &SqlitePool, run: &WorkflowRun) -> anyhow::Result<()> {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(run).await?;
    Commitable::commit(tx).await
}

/// Make every read of `table` fail, so a handler meets a database error.
pub async fn break_table(pool: &SqlitePool, table: &str) -> anyhow::Result<()> {
    sqlx::query(&format!("ALTER TABLE {table} RENAME TO {table}_gone"))
        .execute(pool)
        .await?;
    Ok(())
}

/// What a handler answered.
#[derive(Debug)]
pub struct Answer {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: String,
}

impl Answer {
    /// Assert a JSON `ErrorResponse` with `status` whose text is exactly
    /// `error`.
    #[track_caller]
    pub fn assert_error(&self, status: StatusCode, error: &str) {
        assert_eq!(self.status, status, "{}", self.body);
        assert_eq!(self.content_type.as_deref(), Some("application/json"));
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
    let response = test::call_service(&app, request).await;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = String::from_utf8_lossy(&test::read_body(response).await).into_owned();
    Answer {
        status,
        content_type,
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
