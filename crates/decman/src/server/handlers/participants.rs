//! Participant status and the ACS import quarantine endpoints.

use std::time::Instant;

use actix_web::{HttpRequest, HttpResponse, Responder, delete, get, web};
use sqlx::SqlitePool;

use crate::{
    build_info::{SEMVER, build_version},
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    db::schema::{SchemaRead, SchemaWrite},
    error::Result,
    noise::{
        Message, MessageType, NoiseError, NoiseKeypair, WIRE_VERSION, parse_public_key,
        send_noise_message_with_retry,
    },
    server::{
        AppState,
        health::classify_health_reply,
        middleware::require_admin,
        types::{ConnectionStatus, ErrorResponse, ParticipantStatus, ParticipantsStatusResponse},
    },
};

/// Which participant is quarantined for which party.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub struct QuarantineQuery {
    /// The decentralized party the failed replication was for.
    pub party_id: String,
    /// The participant that may hold part of its ACS.
    pub participant_id: String,
}

/// Lift an ACS-import quarantine.
///
/// A transfer that failed after bytes had reached Canton leaves the
/// participant holding an unknown fraction of the party's ACS, and Canton
/// offers no way to ask how much. Replication onto it is refused until an
/// operator confirms it has been repaired or restored, which is what this
/// records. It does not repair anything.
#[utoipa::path(
    tag = "Parties",
    params(QuarantineQuery),
    responses(
        (status = 200, description = "Quarantine lifted, or none was set"),
        (status = 400, description = "Malformed party or participant id", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[delete("/acs-import-quarantine")]
pub async fn clear_acs_import_quarantine(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    query: web::Query<QuarantineQuery>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let (Ok(party_id), Ok(participant_id)) = (
        CantonId::parse(&query.party_id),
        CantonId::parse(&query.participant_id),
    ) else {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "party_id and participant_id must be valid Canton ids".to_string(),
        });
    };

    match data
        .db
        .clear_acs_import_quarantine(&party_id, &participant_id)
        .await
    {
        Ok(lifted) => {
            if lifted {
                tracing::warn!(
                    "ACS-import quarantine lifted for {participant_id} on {party_id} — \
                     an operator has declared the participant repaired"
                );
            }
            HttpResponse::Ok().json(serde_json::json!({ "lifted": lifted }))
        }
        Err(e) => HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to lift the quarantine: {e}"),
        }),
    }
}

/// Check connectivity status of all participants
#[utoipa::path(
    tag = "Parties",
    responses(
        (status = 200, description = "Participants connection status", body = ParticipantsStatusResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/participants-status")]
pub async fn get_participants_status(data: web::Data<AppState>) -> impl Responder {
    match check_participants_status(&data.config, &data.db).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(e) => {
            tracing::error!("Failed to check participants status: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to check participants status: {e}"),
            })
        }
    }
}

async fn check_participants_status(
    config: &NodeConfig,
    db: &SqlitePool,
) -> Result<ParticipantsStatusResponse> {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let current_participant_id = config.participant_id();
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let mut status_futures = Vec::new();

    for peer in network_config.peers.iter() {
        let peer_id = peer.participant_id.to_string();
        let is_self = peer.participant_id == *current_participant_id;

        if is_self {
            status_futures.push(tokio::spawn(async move {
                ParticipantStatus {
                    id: peer_id,
                    status: ConnectionStatus::CurrentNode,
                    latency_ms: None,
                    workflow: None,
                    version: Some(SEMVER.to_string()),
                    build_version: Some(build_version().to_string()),
                    wire_version: Some(WIRE_VERSION),
                }
            }));
            continue;
        }

        let peer_pub_key = parse_public_key(&peer.public_key).ok();
        let psk = peer_pub_key.map(|pk| keypair.derive_psk(&pk));
        let identity = current_participant_id.to_string();
        let address = peer.address.clone();
        let port = peer.port;
        let noise_retry_cfg = config.noise_retry.clone();

        status_futures.push(tokio::spawn(async move {
            let (Some(psk), Some(_)) = (psk, peer_pub_key) else {
                // Public key parse failed — no PSK available; classify as handshake-side.
                return ParticipantStatus {
                    id: peer_id,
                    status: ConnectionStatus::HandshakeFailed,
                    latency_ms: None,
                    workflow: None,
                    version: None,
                    build_version: None,
                    wire_version: None,
                };
            };

            let started = Instant::now();
            match send_noise_message_with_retry(
                &address,
                port,
                &psk,
                identity.as_bytes(),
                &Message::new_empty(MessageType::Health),
                &noise_retry_cfg,
            )
            .await
            {
                Ok(response) => {
                    // A successful Noise round-trip means the peer is reachable;
                    // classify_health_reply extracts its workflow state (or None
                    // if the peer is on older code that doesn't answer Health).
                    let latency_ms = u64::try_from(started.elapsed().as_millis()).ok();
                    let reply = classify_health_reply(&response);
                    ParticipantStatus {
                        id: peer_id,
                        status: reply.status,
                        latency_ms,
                        workflow: reply.workflow,
                        version: reply.version,
                        build_version: reply.build_version,
                        wire_version: Some(WIRE_VERSION),
                    }
                }
                Err(e) => {
                    // Map NoiseError -> ConnectionStatus (binary semantics — Unreachable
                    // covers transport-side failures; HandshakeFailed covers everything
                    // else). A version mismatch stays HandshakeFailed so older clients
                    // still read it; `wire_version` tells it apart.
                    let (status, wire_version) = match &e {
                        NoiseError::TcpConnectionTimeout(_)
                        | NoiseError::TcpConnectionFailed(_)
                        | NoiseError::Io(_)
                        | NoiseError::Hyper(_)
                        | NoiseError::RequestTimeout => (ConnectionStatus::Unreachable, None),
                        NoiseError::WireVersionMismatch { peer } => {
                            (ConnectionStatus::HandshakeFailed, Some(*peer))
                        }
                        _ => (ConnectionStatus::HandshakeFailed, None),
                    };
                    ParticipantStatus {
                        id: peer_id,
                        status,
                        latency_ms: None,
                        workflow: None,
                        version: None,
                        build_version: None,
                        wire_version,
                    }
                }
            }
        }));
    }

    let results = futures::future::join_all(status_futures).await;
    let statuses: Vec<ParticipantStatus> = results.into_iter().filter_map(|r| r.ok()).collect();

    Ok(ParticipantsStatusResponse {
        statuses,
        wire_version: Some(WIRE_VERSION),
    })
}

#[cfg(test)]
mod clear_acs_import_quarantine_gate_tests {
    use actix_web::{
        App,
        http::StatusCode,
        test::{self, TestRequest},
    };

    use super::*;
    use crate::server::middleware::AuthMiddleware;

    fn clear_request() -> TestRequest {
        let namespace = "0".repeat(68);
        TestRequest::delete().uri(&format!(
            "/acs-import-quarantine?party_id=dec-party::{namespace}\
             &participant_id=participant::{namespace}"
        ))
    }

    /// The gate fires before the handler touches the quarantine table.
    #[actix_web::test]
    async fn a_request_without_a_principal_is_unauthorized() -> anyhow::Result<()> {
        let data = AppState::for_test(None).await?;
        let app = test::init_service(
            App::new()
                .app_data(data)
                .service(clear_acs_import_quarantine),
        )
        .await;

        let resp = test::call_service(&app, clear_request().to_request()).await;

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    /// A valid token without the admin role must not lift the interlock that
    /// keeps replication off a participant holding a partial ACS.
    #[actix_web::test]
    async fn a_non_admin_cannot_lift_the_quarantine() -> anyhow::Result<()> {
        let data = AppState::for_test_as_non_admin().await?;
        let app = test::init_service(
            App::new()
                .app_data(data)
                .wrap(AuthMiddleware)
                .service(clear_acs_import_quarantine),
        )
        .await;

        let req = clear_request()
            .insert_header(("Authorization", "Bearer spa.user.token"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use http::StatusCode;
    use tokio::{net::TcpListener, task::JoinHandle};
    use tokio_noise::handshakes::nn_psk2::Responder as NoiseResponder;

    use super::*;
    use crate::{
        config::{NoiseRetryConfig, Peer},
        db::{MIGRATOR, schema::Commitable},
    };

    /// A peer listener that answers every request with `status` and `body`
    /// over a real Noise session. Returns its port and how many requests it
    /// served.
    async fn stub_peer(
        psk: [u8; 32],
        status: StatusCode,
        body: Vec<u8>,
    ) -> anyhow::Result<(u16, Arc<AtomicUsize>, JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let served = Arc::new(AtomicUsize::new(0));
        let counter = served.clone();
        let server = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let counter = counter.clone();
                let body = body.clone();
                tokio::spawn(async move {
                    let respond = move |_: &[u8], _: hyper::Request<hyper::Body>| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let mut response = hyper::Response::new(hyper::Body::from(body.clone()));
                        *response.status_mut() = status;
                        async move { Ok::<_, hyper::Error>(response) }
                    };
                    let responder = NoiseResponder::new(move |_: &[u8]| Some(psk));
                    if let Err(e) =
                        hyper_noise::server::serve_http(socket, responder, respond, None).await
                    {
                        tracing::warn!("stub peer connection failed: {e}");
                    }
                });
            }
        });
        Ok((port, served, server))
    }

    /// A refusing peer stays `HandshakeFailed` either way. Only a refusal on
    /// another wire version carries a byte, so the hover can tell the two
    /// apart, and only that refusal is not retried.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_status_probe_reports_only_a_mismatched_peers_wire_version(
        db: SqlitePool,
    ) -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let mut config = NodeConfig::default().with_root_dir(root.path());
        let self_id = CantonId::parse(&format!("self::1220{}", "a".repeat(64)))?;
        config.node.participant_id = Some(self_id.clone());
        config.noise_retry = NoiseRetryConfig {
            per_attempt_timeout_secs: 5,
            max_attempts: 2,
            backoff_ms: 0,
        };
        let keys = NoiseKeypair::generate();
        tokio::fs::create_dir_all(config.data_dir()).await?;
        keys.save_to_file(config.key_file_path()).await?;

        let older = WIRE_VERSION - 1;
        let mut mismatch = Message::new(
            MessageType::Error,
            b"request rejected: Noise protocol version mismatch".to_vec(),
        )
        .to_bytes();
        mismatch[0] = older;
        let denied = Message::new(
            MessageType::Error,
            b"request rejected: unknown sender".to_vec(),
        )
        .to_bytes();
        let pong = Message::new_empty(MessageType::Pong).to_bytes();

        let mut peers = vec![Peer {
            participant_id: self_id,
            name: "self".into(),
            address: "127.0.0.1".into(),
            port: 1,
            public_key: keys.public_key_hex(),
            party: None,
        }];
        let mut stubs = HashMap::new();
        for (name, fill, status, body) in [
            ("older", 'b', StatusCode::SERVICE_UNAVAILABLE, mismatch),
            ("denying", 'c', StatusCode::SERVICE_UNAVAILABLE, denied),
            ("healthy", 'd', StatusCode::OK, pong),
        ] {
            let peer_keys = NoiseKeypair::generate();
            let psk = *peer_keys.derive_psk(&keys.public_key);
            let (port, served, server) = stub_peer(psk, status, body).await?;
            peers.push(Peer {
                participant_id: CantonId::parse(&format!(
                    "{name}::1220{}",
                    fill.to_string().repeat(64)
                ))?,
                name: name.into(),
                address: "127.0.0.1".into(),
                port,
                public_key: peer_keys.public_key_hex(),
                party: None,
            });
            stubs.insert(name, (served, server));
        }
        let mut tx = db.begin_transaction().await?;
        for peer in &peers {
            tx.insert_peer(peer).await?;
        }
        Commitable::commit(tx).await?;

        let response = check_participants_status(&config, &db).await?;
        for (_, server) in stubs.values() {
            server.abort();
        }

        let row = |name: &str| {
            response
                .statuses
                .iter()
                .find(|s| s.id.starts_with(&format!("{name}::")))
                .map(|s| (s.status, s.wire_version))
                .ok_or_else(|| anyhow::anyhow!("no status row for {name}"))
        };
        let served = |name: &str| stubs.get(name).map(|(n, _)| n.load(Ordering::SeqCst));
        assert_eq!(response.wire_version, Some(WIRE_VERSION));
        assert_eq!(
            row("self")?,
            (ConnectionStatus::CurrentNode, Some(WIRE_VERSION))
        );
        assert_eq!(
            row("older")?,
            (ConnectionStatus::HandshakeFailed, Some(older))
        );
        assert_eq!(row("denying")?, (ConnectionStatus::HandshakeFailed, None));
        assert_eq!(
            row("healthy")?,
            (ConnectionStatus::Connected, Some(WIRE_VERSION))
        );
        assert_eq!(served("older"), Some(1));
        assert_eq!(served("denying"), Some(config.noise_retry.max_attempts));
        Ok(())
    }
}
