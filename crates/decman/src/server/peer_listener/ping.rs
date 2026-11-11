//! The peer ping loop that records which peers answer.

use std::{collections::HashMap, sync::Arc, time::Duration};

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::server::peer_status;
use crate::{
    config::NodeConfig,
    db::schema::SchemaRead,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key},
    server::peer_status::LastSeen,
};

/// Ping peers every 5 seconds
pub(super) async fn run_peer_ping_loop(
    config: NodeConfig,
    db: SqlitePool,
    peer_status: Arc<RwLock<HashMap<String, bool>>>,
    last_seen: LastSeen,
    keypair: Arc<NoiseKeypair>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let peers = match db.get_all_peers().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load peers from database for heartbeat");
                continue;
            }
        };

        let our_canton_id = config.participant_id();
        let our_id = our_canton_id.to_string();
        let futures: Vec<_> = peers
            .iter()
            .filter(|p| &p.participant_id != our_canton_id)
            .map(|peer| {
                let id = peer.participant_id.to_string();
                let address = peer.address.clone();
                let port = peer.port;
                let public_key_hex = peer.public_key.clone();
                let last_seen = last_seen.clone();
                let keypair = keypair.clone();
                let our_id = our_id.clone();

                async move {
                    // Parse the peer's pubkey from the freshly-loaded DB row.
                    // (Cannot rely on a startup-only cache: new peers must be probed.)
                    let peer_pubkey = match parse_public_key(&public_key_hex) {
                        Ok(pk) => pk,
                        Err(e) => return (id, Err(format!("malformed public_key: {e}"))),
                    };

                    let now = std::time::Instant::now();
                    let stale = {
                        let map = last_seen.read().await;
                        peer_status::should_probe(&map, &id, now)
                    };

                    if !stale {
                        return (id, Ok(()));
                    }

                    let psk = keypair.derive_psk(&peer_pubkey);
                    let result = crate::noise::send_noise_message(
                        &address,
                        port,
                        &psk,
                        our_id.as_bytes(),
                        &Message::new_empty(MessageType::Ping),
                    )
                    .await;

                    let verdict = match result {
                        Ok(bytes) => match Message::from_bytes(&bytes) {
                            Ok(m) if m.msg_type == MessageType::Pong => Ok(()),
                            Ok(m) => Err(format!("answered {:?} instead of Pong", m.msg_type)),
                            Err(e) => Err(format!("unreadable reply: {e}")),
                        },
                        Err(e) => Err(e.to_string()),
                    };

                    if verdict.is_ok() {
                        let now = std::time::Instant::now();
                        let mut map = last_seen.write().await;
                        peer_status::bump(&mut map, id.clone(), now);
                    }

                    let verdict = {
                        let map = last_seen.read().await;
                        peer_status::settle_probe(&map, &id, std::time::Instant::now(), verdict)
                    };
                    (id, verdict)
                }
            })
            .collect();

        let verdicts = futures::future::join_all(futures).await;
        peer_status::record_verdicts(&mut *peer_status.write().await, verdicts);
    }
}
