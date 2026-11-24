//! The always-on Noise listener task and the peer key map it serves.

use std::{collections::HashMap, sync::Arc, time::Duration};

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use super::{
    connection::handle_incoming_connection, ping::run_peer_ping_loop, triggers::WorkflowTriggers,
};
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    db::schema::SchemaRead,
    noise::{load_or_generate_keypair, parse_public_key},
    server::{peer_status::LastSeen, startup::spawn_supervised},
};

/// Background task that runs a Noise server for handling pings and invites
pub(crate) async fn run_heartbeat(
    config: NodeConfig,
    db: SqlitePool,
    peer_status: Arc<RwLock<HashMap<String, bool>>>,
    last_seen: LastSeen,
    triggers: WorkflowTriggers,
) {
    use tokio::net::TcpListener;

    let listen_addr = format!(
        "{addr}:{port}",
        addr = config.node.listen_address,
        port = config.node.port
    );

    // Load or generate keypair for Noise handshakes
    let keypair = match load_or_generate_keypair(&config.key_file_path()).await {
        Ok(kp) => Arc::new(kp),
        Err(e) => {
            tracing::error!("Failed to load or generate keypair: {e}");
            return;
        }
    };
    // Surface our own Noise public key at startup so operators can confirm the
    // running node is using the key it published to peers. A mismatch fails
    // every handshake symmetrically and is otherwise invisible in the logs.
    tracing::info!("Noise public key: {key}", key = keypair.public_key_hex());

    // Peer keys for inbound authentication are resolved LIVE from the DB on
    // each incoming connection (see `handle_incoming_connection`) rather than
    // snapshotted here. A frozen snapshot meant that after a `/network-config`
    // key correction the node would accept a peer *outbound* (which reads the
    // live DB) but keep rejecting it *inbound* until a process restart —
    // producing asymmetric red/green across the mesh. Resolving live makes a
    // key fix take effect for both directions immediately.
    let self_id = config.participant_id().clone();

    // Listener loop: bind the always-on Noise listener and accept forever. It is
    // never paused — workflow traffic is routed in-process via the
    // active-workflow slot, so the listener stays up (and keeps answering
    // Health / Ping) even while this node is participating in a workflow.
    let keypair_spawn = keypair.clone();
    let last_seen_spawn = last_seen.clone();
    let db_spawn = db.clone();
    let self_id_spawn = self_id.clone();
    let triggers_spawn = triggers.clone();

    spawn_supervised(
        "noise listener",
        "this node accepts no inbound peer connections until it restarts",
        async move {
            loop {
                match TcpListener::bind(&listen_addr).await {
                    Ok(listener) => {
                        tracing::info!("Noise listener started on {listen_addr}");

                        loop {
                            match listener.accept().await {
                                Ok((socket, peer_addr)) => {
                                    let keypair = keypair_spawn.clone();
                                    let last_seen = last_seen_spawn.clone();
                                    let db = db_spawn.clone();
                                    let self_id = self_id_spawn.clone();
                                    let triggers = triggers_spawn.clone();

                                    tokio::spawn(async move {
                                        handle_incoming_connection(
                                            socket, peer_addr, keypair, db, self_id, triggers,
                                            last_seen,
                                        )
                                        .await;
                                    });
                                }
                                Err(e) => {
                                    // Don't tight-loop on persistent accept errors (e.g. FD
                                    // exhaustion): log and back off briefly before retrying.
                                    tracing::warn!(
                                        "Noise listener accept error on {listen_addr}: {e}; \
                                         backing off 100ms"
                                    );
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to bind Noise listener on {listen_addr}: {e}, retrying in 5s"
                        );
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        },
    );

    // Ping peers every 5 seconds
    run_peer_ping_loop(config, db, peer_status, last_seen, keypair).await;
}

/// Build the identity → public-key allowlist used to authenticate inbound
/// Noise handshakes, from the current `peers` table. Skips self and any peer
/// whose public key is empty or unparseable (the latter is logged so a bad
/// key is visible rather than silently dropped). Read fresh per connection so
/// `/network-config` updates take effect without a restart; a DB error yields
/// an empty allowlist (all inbound handshakes fail closed) rather than a panic.
pub(super) async fn build_peer_key_map(
    db: &SqlitePool,
    self_id: &CantonId,
) -> HashMap<String, secp256k1::PublicKey> {
    let peers = match db.get_all_peers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to load peers for inbound Noise auth: {e}");
            return HashMap::new();
        }
    };
    let mut map = HashMap::new();
    for peer in &peers {
        if peer.participant_id == *self_id || peer.public_key.is_empty() {
            continue;
        }
        match parse_public_key(&peer.public_key) {
            Ok(pub_key) => {
                map.insert(peer.participant_id.to_string(), pub_key);
            }
            Err(e) => tracing::warn!(
                "Skipping inbound auth key for {} — unparseable public key: {e}",
                peer.participant_id
            ),
        }
    }
    map
}
