//! Pins every answer of the peer listener over a real Noise connection: for
//! each message kind, the status, the headers and the reply bytes, and what
//! the message leaves in the pending invitations and the chunk cache.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use hyper::{Body, Request, header::CONNECTION};
use sqlx::SqlitePool;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Mutex, RwLock, mpsc},
    task::JoinHandle,
};
use tokio_noise::handshakes::nn_psk2::Initiator;

use super::{connection::handle_incoming_connection, triggers::WorkflowTriggers};
use crate::{
    canton_id::CantonId,
    config::{NodeConfig, Peer},
    db::{
        MIGRATOR,
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    noise::{Message, MessageType, NoiseKeypair, parse_public_key},
    server::{WorkflowRegistry, health, peer_status::LastSeen, state::PEER_CHUNK_CACHE_TTL},
};

type Result<T = ()> = anyhow::Result<T>;

/// A listener node and a client node that it knows as a peer.
struct Harness {
    _root: tempfile::TempDir,
    listener_keys: NoiseKeypair,
    listener_id: CantonId,
    client_keys: NoiseKeypair,
    client_id: CantonId,
    port: u16,
    db: SqlitePool,
    triggers: WorkflowTriggers,
    last_seen: LastSeen,
    task: JoinHandle<()>,
}

impl Harness {
    async fn start(db: SqlitePool) -> Result<Self> {
        let root = tempfile::tempdir()?;
        let mut config = NodeConfig::default().with_root_dir(root.path());
        let listener_id = CantonId::parse(&format!("listener::1220{}", "a".repeat(64)))?;
        config.node.participant_id = Some(listener_id.clone());
        // Nothing listens there, so a Canton read fails at once.
        config.canton.admin_api_port = closed_port()?;
        config.canton.ledger_api_port = closed_port()?;
        tokio::fs::create_dir_all(config.data_dir()).await?;
        let listener_keys = NoiseKeypair::generate();
        listener_keys.save_to_file(config.key_file_path()).await?;

        let client_keys = NoiseKeypair::generate();
        let client_id = CantonId::parse(&format!("client::1220{}", "b".repeat(64)))?;
        let mut tx = db.begin_transaction().await?;
        tx.insert_peer(&Peer {
            participant_id: client_id.clone(),
            name: "client".into(),
            address: "127.0.0.1".into(),
            port: 1,
            public_key: client_keys.public_key_hex(),
            party: None,
        })
        .await?;
        Commitable::commit(tx).await?;

        let triggers = WorkflowTriggers {
            pending_invitations: Arc::new(RwLock::new(Vec::new())),
            config: config.clone(),
            peer_chunk_cache: Arc::new(Mutex::new(HashMap::new())),
            db: db.clone(),
            party_credentials: Arc::new(RwLock::new(Vec::new())),
            workflows: WorkflowRegistry::new(),
            peer_job_sender: mpsc::unbounded_channel().0,
        };
        let last_seen: LastSeen = Arc::new(RwLock::new(HashMap::new()));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let keypair = Arc::new(NoiseKeypair::from_file(config.key_file_path()).await?);
        let task = {
            let (db, triggers, last_seen, self_id) = (
                db.clone(),
                triggers.clone(),
                last_seen.clone(),
                listener_id.clone(),
            );
            tokio::spawn(async move {
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
            })
        };
        Ok(Self {
            _root: root,
            listener_keys,
            listener_id,
            client_keys,
            client_id,
            port,
            db,
            triggers,
            last_seen,
            task,
        })
    }

    /// Send `body` as the client and describe the answer in one line.
    async fn exchange(&self, label: &str, body: Vec<u8>) -> Result<String> {
        let stream = TcpStream::connect(("127.0.0.1", self.port)).await?;
        let psk = self
            .client_keys
            .derive_psk(&parse_public_key(&self.listener_keys.public_key_hex())?);
        let identity = self.client_id.to_string();
        let request = Request::builder()
            .uri(format!("http://127.0.0.1:{}/message", self.port))
            .method("POST")
            .header(CONNECTION, "close")
            .body(Body::from(body))?;
        let response = hyper_noise::client::send_request(
            stream,
            Initiator {
                psk: psk.as_slice(),
                identity: identity.as_bytes(),
            },
            request,
            Some(Duration::from_secs(10)),
        )
        .await?;
        let status = response.status();
        let headers: Vec<String> = response
            .headers()
            .iter()
            .filter(|(name, _)| *name != hyper::header::DATE)
            .map(|(name, value)| format!("{name}={}", value.to_str().unwrap_or("?")))
            .collect();
        let bytes = hyper::body::to_bytes(response.into_body()).await?;
        Ok(self.redact(&format!(
            "{label}: {status} [{headers}] {body}",
            headers = headers.join(", "),
            body = describe(&bytes)?
        )))
    }

    /// Send `message` as the client.
    async fn send(&self, label: &str, message: &Message) -> Result<String> {
        self.exchange(label, message.to_bytes()).await
    }

    /// Replace the random keys with names, so a line reads the same on
    /// every run.
    fn redact(&self, line: &str) -> String {
        let client = self.client_keys.public_key_hex();
        let listener = self.listener_keys.public_key_hex();
        line.replace(&client, "<client-key>")
            .replace(&client[..16], "<client-key-16>")
            .replace(&listener, "<listener-key>")
    }

    /// The pending invitations in memory and in the database, without
    /// their receive time.
    async fn invitations(&self) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        for invitation in self.triggers.pending_invitations.read().await.iter() {
            let mut json = serde_json::to_value(invitation)?;
            if let Some(object) = json.as_object_mut() {
                object.remove("received_at");
            }
            lines.push(self.redact(&format!("memory {json}")));
        }
        let mut stored: Vec<String> = self
            .db
            .get_all_pending_invitations()
            .await?
            .into_iter()
            .map(|invitation| self.redact(&format!("db {}", invitation.id)))
            .collect();
        stored.sort();
        lines.extend(stored);
        Ok(lines)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn closed_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

/// A reply body in one line: empty, or the message it decodes to. The
/// message must encode back to the same bytes, so the line pins them.
fn describe(bytes: &[u8]) -> Result<String> {
    if bytes.is_empty() {
        return Ok("empty".to_string());
    }
    let message = Message::from_bytes(bytes)?;
    anyhow::ensure!(
        message.to_bytes() == bytes,
        "the reply is not a canonical frame"
    );
    Ok(format!(
        "{:?} {:?} {:?}",
        message.msg_type,
        message.instance,
        String::from_utf8_lossy(&message.payload)
    ))
}

fn id(prefix: &str) -> String {
    format!("{prefix}::1220{}", "c".repeat(64))
}

/// A valid payload for each invite kind, with run instance `run-<kind>`.
fn invite_payloads() -> Vec<(MessageType, String)> {
    let (dec, p1, p2) = (id("dec"), id("p1"), id("p2"));
    vec![
        (
            MessageType::InviteOnboarding,
            format!(
                r#"{{"prefix":"newparty","participants":["{p1}","{p2}"],"threshold":2,"workflow_instance":"run-onboarding"}}"#
            ),
        ),
        (
            MessageType::InviteKick,
            format!(
                r#"{{"dec_party_id":"{dec}","kicked_participant":"{p2}","kicked_owner_key":"k2","new_threshold":1,"previous_threshold":2,"confirmation_threshold":1,"participants":["{p1}"],"workflow_instance":"run-kick"}}"#
            ),
        ),
        (
            MessageType::InviteContracts,
            format!(
                r#"{{"dec_party_id":"{dec}","participants":["{p2}"],"package_names":["Rules"],"contract_intents":[{{"package_id":"pkg","module_name":"Governance.Rules","entity_name":"GovernanceRules","argument_hash":"hash"}}],"workflow_instance":"run-contracts"}}"#
            ),
        ),
        (
            MessageType::InviteDars,
            format!(
                r#"{{"dar_filenames":["app.dar"],"dar_hashes":["hash"],"participants":["{p2}"],"workflow_instance":"run-dars"}}"#
            ),
        ),
        (
            MessageType::InviteAddParty,
            format!(
                r#"{{"dec_party_id":"{dec}","new_participant":"{p2}","new_threshold":2,"previous_threshold":1,"participants":["{p1}","{p2}"],"workflow_instance":"run-add-party"}}"#
            ),
        ),
        (
            MessageType::InviteChangeThreshold,
            format!(
                r#"{{"dec_party_id":"{dec}","new_threshold":2,"previous_threshold":1,"confirmation_threshold":1,"participants":["{p1}","{p2}"],"workflow_instance":"run-change-threshold"}}"#
            ),
        ),
        (
            MessageType::InviteRotateKey,
            format!(
                r#"{{"dec_party_id":"{dec}","rotated_key":"1220{key}","threshold":2,"confirmation_threshold":1,"participants":["{p1}","{p2}"],"workflow_instance":"run-rotate-key"}}"#,
                key = "d".repeat(64)
            ),
        ),
    ]
}

/// The kinds the listener routes to a coordinator run.
const ROUTED: [MessageType; 15] = [
    MessageType::GetNextCommand,
    MessageType::GetNextAcsBlock,
    MessageType::KeysUpload,
    MessageType::DnsSignature,
    MessageType::P2pSignatures,
    MessageType::SubmissionSignatures,
    MessageType::KickSignatures,
    MessageType::AddPartyKeysUpload,
    MessageType::AddPartySignatures,
    MessageType::AddPartyClearSignatures,
    MessageType::AddPartyClearProposal,
    MessageType::ChangeThresholdSignatures,
    MessageType::RotateKeySignatures,
    MessageType::StatusUpdate,
    MessageType::DeclineInvitation,
];

#[sqlx::test(migrator = "MIGRATOR")]
async fn each_message_gets_its_answer(db: SqlitePool) -> Result {
    let node = Harness::start(db).await?;
    let mut lines = Vec::new();

    lines.push(node.exchange("short frame", vec![1, 2, 3]).await?);
    lines.push(node.exchange("unparseable frame", vec![0xff; 8]).await?);
    lines.push(
        node.send("ping", &Message::new_empty(MessageType::Ping))
            .await?,
    );
    let health = node
        .send("health", &Message::new_empty(MessageType::Health))
        .await?;
    let expected_health = health::build_health_response(&node.db, &node.listener_id.to_string())
        .await
        .to_payload();
    assert_eq!(
        health,
        format!(
            "health: 200 OK [content-length={}] HealthResponse \"\" {:?}",
            Message::new(MessageType::HealthResponse, expected_health.clone())
                .to_bytes()
                .len(),
            String::from_utf8_lossy(&expected_health)
        )
    );
    lines.push(
        node.send(
            "dar without an invitation",
            &Message::new(MessageType::RequestDar, b"0".to_vec()),
        )
        .await?,
    );
    lines.push(
        node.send(
            "chunk of a gone run",
            &Message::new(MessageType::GetChunk, vec![0, 0, 0, 0]).with_instance("gone"),
        )
        .await?,
    );
    lines.push(
        node.send(
            "chunk with a short index",
            &Message::new(MessageType::GetChunk, vec![0, 0]),
        )
        .await?,
    );
    lines.push(
        node.send(
            "chunk with nothing cached",
            &Message::new(MessageType::GetChunk, vec![0, 0, 0, 0]),
        )
        .await?,
    );
    let client_key = node.client_keys.public_key_hex();
    node.triggers.peer_chunk_cache.lock().await.insert(
        client_key.clone(),
        (b"cached payload".to_vec(), Instant::now()),
    );
    lines.push(
        node.send(
            "first chunk",
            &Message::new(MessageType::GetChunk, vec![0, 0, 0, 0]),
        )
        .await?,
    );
    lines.push(
        node.send(
            "chunk past the end",
            &Message::new(MessageType::GetChunk, vec![0, 0, 0, 1]),
        )
        .await?,
    );
    let stale = Instant::now()
        .checked_sub(PEER_CHUNK_CACHE_TTL + Duration::from_secs(1))
        .ok_or_else(|| anyhow::anyhow!("the clock is too young for a stale entry"))?;
    node.triggers
        .peer_chunk_cache
        .lock()
        .await
        .insert(client_key.clone(), (b"cached payload".to_vec(), stale));
    lines.push(
        node.send(
            "chunk of an expired entry",
            &Message::new(MessageType::GetChunk, vec![0, 0, 0, 0]),
        )
        .await?,
    );
    lines.push(format!(
        "cache after expiry: {} entries",
        node.triggers.peer_chunk_cache.lock().await.len()
    ));
    lines.push(
        node.send(
            "owner keys, none asked",
            &Message::new_empty(MessageType::RequestOwnerKeys),
        )
        .await?,
    );
    lines.push(
        node.send(
            "owner keys, Canton down",
            &Message::new(
                MessageType::RequestOwnerKeys,
                format!(r#"["{}"]"#, id("dec")).into_bytes(),
            ),
        )
        .await?,
    );
    lines.push(
        node.send("peer list", &Message::new_empty(MessageType::ListPeers))
            .await?,
    );
    lines.push(
        node.send(
            "member party, unknown",
            &Message::new(MessageType::RequestMemberParty, id("dec").into_bytes()),
        )
        .await?,
    );
    for kind in ROUTED {
        lines.push(
            node.send(
                &format!("{kind:?} for a gone run"),
                &Message::new(kind, Vec::new()).with_instance("gone"),
            )
            .await?,
        );
    }
    for (kind, payload) in invite_payloads() {
        lines.push(
            node.send(
                &format!("{kind:?} without a payload"),
                &Message::new_empty(kind),
            )
            .await?,
        );
        lines.push(
            node.send(
                &format!("{kind:?} with a bad payload"),
                &Message::new(kind, b"not json".to_vec()),
            )
            .await?,
        );
        lines.push(
            node.send(
                &format!("{kind:?}"),
                &Message::new(kind, payload.into_bytes()),
            )
            .await?,
        );
    }
    lines.extend(node.invitations().await?);
    lines.push(
        node.send(
            "cancel one run",
            &Message::new_empty(MessageType::CancelInvite).with_instance("run-dars"),
        )
        .await?,
    );
    lines.push(
        node.send(
            "retry one run",
            &Message::new_empty(MessageType::RetryWorkflow).with_instance("run-kick"),
        )
        .await?,
    );
    lines.push(
        node.send("unhandled kind", &Message::new_empty(MessageType::Pong))
            .await?,
    );
    lines.extend(node.invitations().await?);
    let seen: Vec<String> = node.last_seen.read().await.keys().cloned().collect();
    lines.push(format!("last seen: {seen:?}"));

    let expected: Vec<String> = EXPECTED.lines().map(str::to_string).collect();
    assert_eq!(lines, expected, "actual:\n{}", lines.join("\n"));
    Ok(())
}

const EXPECTED: &str = r#"short frame: 200 OK [content-length=0] empty
unparseable frame: 503 Service Unavailable [content-length=78] Error "" "request rejected: Message too short: expected at least 9 bytes, got 8"
ping: 200 OK [content-length=75] Pong "" "<listener-key>"
dar without an invitation: 200 OK [content-length=118] Error "" "malformed RequestDar payload: invalid type: integer `0`, expected struct RequestDarPayload at line 1 column 1"
chunk of a gone run: 503 Service Unavailable [content-length=80] Error "" "no such workflow run on this peer: it was cancelled or already finished"
chunk with a short index: 400 Bad Request [content-length=0] empty
chunk with nothing cached: 404 Not Found [content-length=0] empty
first chunk: 200 OK [content-length=27] Chunk "" "\0\0\0\0cached payload"
chunk past the end: 400 Bad Request [content-length=0] empty
chunk of an expired entry: 404 Not Found [content-length=0] empty
cache after expiry: 0 entries
owner keys, none asked: 200 OK [content-length=11] OwnerKeys "" "[]"
owner keys, Canton down: 200 OK [content-length=11] OwnerKeys "" "[]"
peer list: 200 OK [content-length=89] PeerList "" "[\"client::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"]"
member party, unknown: 200 OK [content-length=9] MemberPartyResponse "" ""
GetNextCommand for a gone run: 503 Service Unavailable [content-length=0] empty
GetNextAcsBlock for a gone run: 503 Service Unavailable [content-length=0] empty
KeysUpload for a gone run: 503 Service Unavailable [content-length=0] empty
DnsSignature for a gone run: 503 Service Unavailable [content-length=0] empty
P2pSignatures for a gone run: 503 Service Unavailable [content-length=0] empty
SubmissionSignatures for a gone run: 503 Service Unavailable [content-length=0] empty
KickSignatures for a gone run: 503 Service Unavailable [content-length=0] empty
AddPartyKeysUpload for a gone run: 503 Service Unavailable [content-length=0] empty
AddPartySignatures for a gone run: 503 Service Unavailable [content-length=0] empty
AddPartyClearSignatures for a gone run: 503 Service Unavailable [content-length=0] empty
AddPartyClearProposal for a gone run: 503 Service Unavailable [content-length=0] empty
ChangeThresholdSignatures for a gone run: 503 Service Unavailable [content-length=0] empty
RotateKeySignatures for a gone run: 503 Service Unavailable [content-length=0] empty
StatusUpdate for a gone run: 503 Service Unavailable [content-length=0] empty
DeclineInvitation for a gone run: 503 Service Unavailable [content-length=0] empty
InviteOnboarding without a payload: 200 OK [content-length=9] Ack "" ""
InviteOnboarding with a bad payload: 200 OK [content-length=9] Ack "" ""
InviteOnboarding: 200 OK [content-length=9] Ack "" ""
InviteKick without a payload: 200 OK [content-length=9] Ack "" ""
InviteKick with a bad payload: 200 OK [content-length=9] Ack "" ""
InviteKick: 200 OK [content-length=9] Ack "" ""
InviteContracts without a payload: 200 OK [content-length=9] Ack "" ""
InviteContracts with a bad payload: 200 OK [content-length=9] Ack "" ""
InviteContracts: 200 OK [content-length=9] Ack "" ""
InviteDars without a payload: 200 OK [content-length=9] Ack "" ""
InviteDars with a bad payload: 200 OK [content-length=9] Ack "" ""
InviteDars: 200 OK [content-length=9] Ack "" ""
InviteAddParty without a payload: 200 OK [content-length=9] Ack "" ""
InviteAddParty with a bad payload: 200 OK [content-length=9] Ack "" ""
InviteAddParty: 200 OK [content-length=9] Ack "" ""
InviteChangeThreshold without a payload: 200 OK [content-length=9] Ack "" ""
InviteChangeThreshold with a bad payload: 200 OK [content-length=9] Ack "" ""
InviteChangeThreshold: 200 OK [content-length=9] Ack "" ""
InviteRotateKey without a payload: 200 OK [content-length=9] Ack "" ""
InviteRotateKey with a bad payload: 200 OK [content-length=9] Ack "" ""
InviteRotateKey: 200 OK [content-length=9] Ack "" ""
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"onboarding-<client-key-16>","invitation_type":"Onboarding"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"onboarding-<client-key-16>-run-onboarding","invitation_type":"Onboarding","new_threshold":2,"participants":["p1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"prefix":"newparty","workflow_instance":"run-onboarding"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"kick-<client-key-16>","invitation_type":"Kick"}
memory {"confirmation_threshold":1,"coordinator_name":null,"coordinator_pubkey":"<client-key>","dec_party_id":"dec::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","id":"kick-<client-key-16>-run-kick","invitation_type":"Kick","kicked_owner_key":"k2","kicked_participant":"p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","new_threshold":1,"participants":["p1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"previous_threshold":2,"workflow_instance":"run-kick"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"contracts-<client-key-16>","invitation_type":"Contracts"}
memory {"contract_intents":[{"argument_hash":"hash","entity_name":"GovernanceRules","module_name":"Governance.Rules","package_id":"pkg"}],"coordinator_name":null,"coordinator_pubkey":"<client-key>","dec_party_id":"dec::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","id":"contracts-<client-key-16>-run-contracts","invitation_type":"Contracts","package_names":["Rules"],"participants":["p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"workflow_instance":"run-contracts"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"dars-<client-key-16>","invitation_type":"Dars"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","dar_filenames":["app.dar"],"dar_hashes":["hash"],"id":"dars-<client-key-16>-run-dars","invitation_type":"Dars","participants":["p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"workflow_instance":"run-dars"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"addparty-<client-key-16>","invitation_type":"AddParty"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","dec_party_id":"dec::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","id":"addparty-<client-key-16>-run-add-party","invitation_type":"AddParty","new_participant":"p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","new_threshold":2,"participants":["p1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"previous_threshold":1,"workflow_instance":"run-add-party"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"changethreshold-<client-key-16>","invitation_type":"ChangeThreshold"}
memory {"confirmation_threshold":1,"coordinator_name":null,"coordinator_pubkey":"<client-key>","dec_party_id":"dec::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","id":"changethreshold-<client-key-16>-run-change-threshold","invitation_type":"ChangeThreshold","new_threshold":2,"participants":["p1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"previous_threshold":1,"workflow_instance":"run-change-threshold"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"rotatekey-<client-key-16>","invitation_type":"RotateKey"}
memory {"confirmation_threshold":1,"coordinator_name":null,"coordinator_pubkey":"<client-key>","dec_party_id":"dec::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","id":"rotatekey-<client-key-16>-run-rotate-key","invitation_type":"RotateKey","new_threshold":2,"participants":["p1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"rotated_key":"1220dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","workflow_instance":"run-rotate-key"}
db addparty-<client-key-16>
db addparty-<client-key-16>-run-add-party
db changethreshold-<client-key-16>
db changethreshold-<client-key-16>-run-change-threshold
db contracts-<client-key-16>
db contracts-<client-key-16>-run-contracts
db dars-<client-key-16>
db dars-<client-key-16>-run-dars
db kick-<client-key-16>
db kick-<client-key-16>-run-kick
db onboarding-<client-key-16>
db onboarding-<client-key-16>-run-onboarding
db rotatekey-<client-key-16>
db rotatekey-<client-key-16>-run-rotate-key
cancel one run: 200 OK [content-length=9] Ack "" ""
retry one run: 200 OK [content-length=9] Ack "" ""
unhandled kind: 200 OK [content-length=0] empty
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"onboarding-<client-key-16>","invitation_type":"Onboarding"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"onboarding-<client-key-16>-run-onboarding","invitation_type":"Onboarding","new_threshold":2,"participants":["p1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"prefix":"newparty","workflow_instance":"run-onboarding"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"kick-<client-key-16>","invitation_type":"Kick"}
memory {"confirmation_threshold":1,"coordinator_name":null,"coordinator_pubkey":"<client-key>","dec_party_id":"dec::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","id":"kick-<client-key-16>-run-kick","invitation_type":"Kick","kicked_owner_key":"k2","kicked_participant":"p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","new_threshold":1,"participants":["p1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"previous_threshold":2,"workflow_instance":"run-kick"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"contracts-<client-key-16>","invitation_type":"Contracts"}
memory {"contract_intents":[{"argument_hash":"hash","entity_name":"GovernanceRules","module_name":"Governance.Rules","package_id":"pkg"}],"coordinator_name":null,"coordinator_pubkey":"<client-key>","dec_party_id":"dec::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","id":"contracts-<client-key-16>-run-contracts","invitation_type":"Contracts","package_names":["Rules"],"participants":["p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"workflow_instance":"run-contracts"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"dars-<client-key-16>","invitation_type":"Dars"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"addparty-<client-key-16>","invitation_type":"AddParty"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","dec_party_id":"dec::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","id":"addparty-<client-key-16>-run-add-party","invitation_type":"AddParty","new_participant":"p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","new_threshold":2,"participants":["p1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"previous_threshold":1,"workflow_instance":"run-add-party"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"changethreshold-<client-key-16>","invitation_type":"ChangeThreshold"}
memory {"confirmation_threshold":1,"coordinator_name":null,"coordinator_pubkey":"<client-key>","dec_party_id":"dec::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","id":"changethreshold-<client-key-16>-run-change-threshold","invitation_type":"ChangeThreshold","new_threshold":2,"participants":["p1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"previous_threshold":1,"workflow_instance":"run-change-threshold"}
memory {"coordinator_name":null,"coordinator_pubkey":"<client-key>","id":"rotatekey-<client-key-16>","invitation_type":"RotateKey"}
memory {"confirmation_threshold":1,"coordinator_name":null,"coordinator_pubkey":"<client-key>","dec_party_id":"dec::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","id":"rotatekey-<client-key-16>-run-rotate-key","invitation_type":"RotateKey","new_threshold":2,"participants":["p1::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","p2::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"rotated_key":"1220dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","workflow_instance":"run-rotate-key"}
db addparty-<client-key-16>
db addparty-<client-key-16>-run-add-party
db changethreshold-<client-key-16>
db changethreshold-<client-key-16>-run-change-threshold
db contracts-<client-key-16>
db contracts-<client-key-16>-run-contracts
db dars-<client-key-16>
db kick-<client-key-16>
db kick-<client-key-16>-run-kick
db onboarding-<client-key-16>
db onboarding-<client-key-16>-run-onboarding
db rotatekey-<client-key-16>
db rotatekey-<client-key-16>-run-rotate-key
last seen: ["client::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]"#;
