//! One inbound peer connection: every message type the listener answers.

use std::{sync::Arc, time::Instant};

use hyper::{Body, Response, StatusCode};
use sqlx::SqlitePool;
use tokio_noise::handshakes::nn_psk2::Responder;

use super::{
    heartbeat::build_peer_key_map,
    replies::{dar_for_invited_peer, peer_payload_response},
    triggers::{InvitationMeta, WorkflowTriggers},
};
use crate::server::owner_keys::list_my_owner_keys;
use crate::server::state::PEER_CHUNK_CACHE_TTL;
use crate::server::types::{
    AddPartyInvitePayload, ChangeThresholdInvitePayload, ContractsInvitePayload, DarsInvitePayload,
    InvitationType, KickInvitePayload, OnboardingInvitePayload,
};
use crate::server::{health, peer_status};
use crate::{
    canton_id::CantonId,
    db::schema::SchemaRead,
    noise::{CHUNK_SIZE, Message, MessageType, NOISE_HANDLER_TIMEOUT, NoiseKeypair},
    server::peer_status::LastSeen,
};

/// Handle an incoming Noise connection (either ping or invite)
pub(crate) async fn handle_incoming_connection(
    socket: tokio::net::TcpStream,
    peer_addr: std::net::SocketAddr,
    keypair: Arc<NoiseKeypair>,
    db: SqlitePool,
    self_id: CantonId,
    triggers: WorkflowTriggers,
    last_seen: LastSeen,
) {
    // Resolve the identity→public-key allowlist LIVE from the DB for this
    // connection so a `/network-config` key correction authenticates inbound
    // handshakes immediately, without a process restart (see `run_heartbeat`).
    let peer_keys = Arc::new(build_peer_key_map(&db, &self_id).await);
    let keypair_for_closure = keypair.clone();
    let peer_keys_clone = peer_keys.clone();
    let our_public_key_hex = keypair.public_key_hex();

    // Create PSK derivation responder. Identity MUST be the sender's
    // `participant_id` string — looked up in `peer_keys_clone` (the
    // allowlist) before any ECDH happens. Returns the raw [u8; 32] by
    // deref-copy from the Zeroizing wrapper; the wrapper drops here so
    // the in-keypair secret material is the only long-lived copy.
    //
    // The earlier "raw 33-byte compressed public key" fallback was
    // removed: it bypassed the allowlist, letting any keypair-holder
    // complete the handshake and inject Invite* messages (audit finding).
    let responder = Responder::new(move |identity: &[u8]| -> Option<[u8; 32]> {
        let peer_id = std::str::from_utf8(identity).ok()?;
        let peer_pub_key = peer_keys_clone.get(peer_id)?;
        Some(*keypair_for_closure.derive_psk(peer_pub_key))
    });

    let result = hyper_noise::server::serve_http(
        socket,
        responder,
        move |peer_id: &[u8], req: hyper::Request<Body>| {
            let triggers = triggers.clone();
            let our_pubkey = our_public_key_hex.clone();
            let peer_keys = peer_keys.clone();
            let last_seen = last_seen.clone();

            // The responder above only authenticates identities that are
            // valid utf-8 participant_id strings present in `peer_keys` (the
            // raw 33-byte fallback was removed in #136 as an audit finding).
            // We extract `peer_id_str` once so both `peer_pubkey_hex` and the
            // `last_seen` bump below can reuse it; the conservative
            // `peer_keys` re-check on the bump path is defensive against any
            // future responder change.
            let peer_id_str = std::str::from_utf8(peer_id).ok().map(str::to_owned);
            let peer_pubkey_hex = peer_id_str
                .as_deref()
                .and_then(|id| peer_keys.get(id))
                .map(|pk| hex::encode(pk.serialize()));

            async move {
                // Bump last_seen for known peers.
                if let Some(id) = peer_id_str.as_deref()
                    && peer_keys.contains_key(id)
                {
                    let now = std::time::Instant::now();
                    let mut map = last_seen.write().await;
                    peer_status::bump(&mut map, id.to_string(), now);
                }

                let body_bytes = hyper::body::to_bytes(req.into_body()).await?;

                if body_bytes.len() < 6 {
                    return Ok::<_, hyper::Error>(Response::new(Body::empty()));
                }

                // Parse the frame exactly once: this is the always-on listener
                // hot path (5s heartbeats per peer + all workflow/chunk traffic),
                // so re-decoding would needlessly re-allocate the payload Vec and
                // the instance String on every request. Deny anything we can't
                // parse EXPLICITLY — most importantly frames from pre-0.1.9
                // builds, whose wire format predates the version byte. An old
                // coordinator inviting this node lands in the Err arm: warn with
                // the sender's identity so the operator can see who needs
                // upgrading, and answer 503 so the old sender's call fails fast
                // instead of silently vanishing.
                let msg = match Message::from_bytes(&body_bytes) {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::warn!(
                            "Denied Noise request from {sender}: {e}",
                            sender = peer_id_str.as_deref().unwrap_or("<unidentified peer>")
                        );
                        // Answer WITH the reason rather than an empty body.
                        // An empty 503 here is indistinguishable at the
                        // requester from a proxy with no healthy backend, so
                        // the operator on the other side saw only "message too
                        // short" (#332). Status stays 503, so an older
                        // requester behaves exactly as before.
                        let mut resp = Response::new(Body::from(
                            Message::new(
                                MessageType::Error,
                                format!("request rejected: {e}").into_bytes(),
                            )
                            .to_bytes(),
                        ));
                        *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                        return Ok(resp);
                    }
                };

                {
                    tracing::debug!("Received message type {:?}", msg.msg_type);

                    match msg.msg_type {
                        MessageType::Ping => {
                            tracing::debug!("Received ping, responding with pong");
                            let pong = Message::new(MessageType::Pong, our_pubkey.into_bytes());
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(pong.to_bytes()))
                                .unwrap());
                        }
                        MessageType::Health => {
                            // Always answer health — even mid-workflow. Reports
                            // liveness plus this node's in-flight workflow (if
                            // any) so peers don't infer "offline" from a busy node.
                            let resp = health::build_health_response(
                                &triggers.db,
                                &triggers.config.participant_id().to_string(),
                            )
                            .await;
                            let msg = Message::new(MessageType::HealthResponse, resp.to_payload());
                            // `Response::new` defaults to 200 OK and is infallible,
                            // so there is no builder error to unwrap.
                            return Ok(Response::new(Body::from(msg.to_bytes())));
                        }
                        MessageType::RequestDar => {
                            tracing::debug!("Received RequestDar request");
                            let sender = peer_id_str.as_deref();
                            let dar = match dar_for_invited_peer(
                                &triggers.db,
                                sender,
                                &msg.payload,
                            )
                            .await
                            {
                                Ok(bytes) => bytes,
                                Err(e) => {
                                    // Refusals are the interesting case here: a
                                    // peer asking for a DAR of a run it was not
                                    // invited to is worth seeing in the log.
                                    tracing::warn!(
                                        "Refused a DAR to {}: {e:#}",
                                        sender.unwrap_or("<unidentified peer>")
                                    );
                                    let response_msg = Message::new(
                                        MessageType::Error,
                                        format!("{e:#}").into_bytes(),
                                    );
                                    return Ok(Response::new(Body::from(
                                        response_msg.to_bytes(),
                                    )));
                                }
                            };
                            return Ok(peer_payload_response(
                                dar,
                                peer_pubkey_hex.as_deref(),
                                &triggers.peer_chunk_cache,
                                "Requested DAR",
                            )
                            .await);
                        }
                        MessageType::GetChunk => {
                            // GetChunk serves BOTH the chunked RequestDar transfer and a
                            // workflow's chunked command. The routing key disambiguates:
                            // workflow chunk requests are ALWAYS stamped with their
                            // coordinator run's instance (NoiseClient stamps every command),
                            // so a keyed GetChunk routes to exactly that run — and an
                            // EMPTY key means a RequestDar transfer and goes straight to
                            // the chunk cache below. No sole-active fallback here: with
                            // concurrent workflows a node is routinely mid-run while a peer
                            // reads a DAR, and falling back would feed the RequestDar
                            // transfer INTO a live workflow's chunk server.
                            if !msg.instance.is_empty() {
                                let active = triggers.workflows.route(&msg.instance);
                                let peer =
                                    peer_id_str.as_deref().and_then(|s| CantonId::parse(s).ok());
                                let resp = match (active, peer) {
                                    (Some(wf), Some(pid)) => {
                                        wf.handle_command(pid, msg).await.unwrap_or_else(|e| {
                                            Message::new(
                                                MessageType::Error,
                                                format!("{e}").into_bytes(),
                                            )
                                        })
                                    }
                                    (None, _) => {
                                        // The named run is gone (cancelled/finished) —
                                        // 503 so the peer's bounded retry gives up.
                                        // Says so in the body, so the requester
                                        // reports the cause rather than an empty reply.
                                        let mut resp = Response::new(Body::from(
                                            Message::new(
                                                MessageType::Error,
                                                b"no such workflow run on this peer: it was \
                                                  cancelled or already finished"
                                                    .to_vec(),
                                            )
                                            .to_bytes(),
                                        ));
                                        *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                                        return Ok(resp);
                                    }
                                    (Some(_), None) => Message::new_empty(MessageType::Wait),
                                };
                                return Ok(Response::new(Body::from(resp.to_bytes())));
                            }
                            if msg.payload.len() < 4 {
                                tracing::warn!("Received GetChunk request with payload < 4 bytes");
                                return Ok(Response::builder()
                                    .status(StatusCode::BAD_REQUEST)
                                    .body(Body::empty())
                                    .unwrap());
                            }
                            let chunk_index = u32::from_be_bytes([
                                msg.payload[0],
                                msg.payload[1],
                                msg.payload[2],
                                msg.payload[3],
                            ]) as usize;

                            let Some(ref pk) = peer_pubkey_hex else {
                                tracing::warn!(
                                    "Received GetChunk request without identifiable peer pubkey"
                                );
                                return Ok(Response::builder()
                                    .status(StatusCode::BAD_REQUEST)
                                    .body(Body::empty())
                                    .unwrap());
                            };

                            let chunk_bytes = {
                                let mut cache = triggers.peer_chunk_cache.lock().await;

                                // Pre-check expiry without holding a borrow into the entry,
                                // so we can `remove` the expired entry without borrowck pain.
                                let entry_state = cache
                                    .get(pk)
                                    .map(|(_, t)| t.elapsed() >= PEER_CHUNK_CACHE_TTL);
                                match entry_state {
                                    None => {
                                        tracing::warn!(
                                            "GetChunk request from {pk} for chunk {chunk_index} \
                                             but no cached payload"
                                        );
                                        return Ok(Response::builder()
                                            .status(StatusCode::NOT_FOUND)
                                            .body(Body::empty())
                                            .unwrap());
                                    }
                                    Some(true) => {
                                        // Expired — drop the stale entry now so it doesn't
                                        // linger until the next RequestDar-driven `retain`.
                                        cache.remove(pk);
                                        tracing::warn!(
                                            "GetChunk for {pk} chunk {chunk_index}: cache entry \
                                             expired (removed)"
                                        );
                                        return Ok(Response::builder()
                                            .status(StatusCode::NOT_FOUND)
                                            .body(Body::empty())
                                            .unwrap());
                                    }
                                    Some(false) => {}
                                }

                                let (payload, t) = cache.get_mut(pk).expect("checked above");
                                let start = chunk_index * CHUNK_SIZE;
                                if start >= payload.len() {
                                    tracing::warn!(
                                        "GetChunk for {pk} chunk {chunk_index}: out of range \
                                         (start={start}, payload_len={})",
                                        payload.len(),
                                    );
                                    return Ok(Response::builder()
                                        .status(StatusCode::BAD_REQUEST)
                                        .body(Body::empty())
                                        .unwrap());
                                }
                                let end = (start + CHUNK_SIZE).min(payload.len());
                                let bytes = payload[start..end].to_vec();
                                // Extend TTL on successful read so a slow peer
                                // mid-reassembly can't have entries evicted out from
                                // under it just because the original 30s window from
                                // insertion ran out.
                                *t = Instant::now();
                                bytes
                            };

                            // Build Chunk response: [chunk_index:4][chunk_data]
                            let mut response_payload = Vec::with_capacity(4 + chunk_bytes.len());
                            response_payload.extend_from_slice(&(chunk_index as u32).to_be_bytes());
                            response_payload.extend_from_slice(&chunk_bytes);

                            let response_msg = Message::new(MessageType::Chunk, response_payload);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(response_msg.to_bytes()))
                                .unwrap());
                        }
                        MessageType::RequestOwnerKeys => {
                            tracing::debug!("Received RequestOwnerKeys request");
                            // Payload is a JSON array of decentralized party_ids
                            // the caller wants this node's owner_keys for (see
                            // #149: peer no longer enumerates the synchronizer).
                            // A malformed or absent payload is treated as
                            // "nothing requested" — return an empty list rather
                            // than fall back to a slow whole-synchronizer scan.
                            let requested_party_ids: Vec<String> =
                                serde_json::from_slice(&msg.payload).unwrap_or_default();
                            let payload =
                                match list_my_owner_keys(&triggers.config, &requested_party_ids)
                                    .await
                                {
                                    Ok(data) => data,
                                    Err(e) => {
                                        tracing::error!("Failed to list owner keys: {e}");
                                        b"[]".to_vec()
                                    }
                                };
                            let response_msg = Message::new(MessageType::OwnerKeys, payload);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(response_msg.to_bytes()))
                                .unwrap());
                        }
                        MessageType::ListPeers => {
                            tracing::debug!("Received ListPeers request");
                            let payload = match triggers.db.get_all_peers().await {
                                Ok(peers) => {
                                    let ids: Vec<String> = peers
                                        .into_iter()
                                        .map(|p| p.participant_id.to_string())
                                        .collect();
                                    serde_json::to_vec(&ids).unwrap_or_else(|_| b"[]".to_vec())
                                }
                                Err(e) => {
                                    tracing::error!("Failed to list peers: {e}");
                                    b"[]".to_vec()
                                }
                            };
                            let response_msg = Message::new(MessageType::PeerList, payload);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(response_msg.to_bytes()))
                                .unwrap());
                        }
                        MessageType::RequestMemberParty => {
                            let dec_party_id =
                                std::str::from_utf8(&msg.payload).unwrap_or("").to_string();
                            tracing::debug!("Received RequestMemberParty for {dec_party_id}",);
                            let payload = {
                                let creds = triggers.party_credentials.read().await;
                                creds
                                    .iter()
                                    .find(|c| c.dec_party_id.to_string() == dec_party_id)
                                    .map(|c| c.member_party_id.to_string().into_bytes())
                                    .unwrap_or_default()
                            };
                            let response_msg =
                                Message::new(MessageType::MemberPartyResponse, payload);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(response_msg.to_bytes()))
                                .unwrap());
                        }
                        MessageType::GetNextCommand
                        | MessageType::GetNextAcsBlock
                        | MessageType::KeysUpload
                        | MessageType::DnsSignature
                        | MessageType::P2pSignatures
                        | MessageType::SubmissionSignatures
                        | MessageType::KickSignatures
                        | MessageType::AddPartyKeysUpload
                        | MessageType::AddPartySignatures
                        | MessageType::AddPartyClearSignatures
                        | MessageType::AddPartyClearProposal
                        | MessageType::ChangeThresholdSignatures
                        | MessageType::StatusUpdate
                        | MessageType::DeclineInvitation => {
                            // Route workflow-command traffic to the coordinator
                            // run identified by `msg.instance` (the peer stamps
                            // its coordinator's run name). The registry lock is
                            // held only long enough to clone the handle out —
                            // never across the await.
                            let active = triggers.workflows.route(&msg.instance);
                            let peer = peer_id_str.as_deref().and_then(|s| CantonId::parse(s).ok());
                            let resp = match (active, peer) {
                                (Some(wf), Some(pid)) => {
                                    wf.handle_command(pid, msg).await.unwrap_or_else(|e| {
                                        Message::new(
                                            MessageType::Error,
                                            format!("{e}").into_bytes(),
                                        )
                                    })
                                }
                                (None, _) => {
                                    // No matching coordinator run on this node. The peer is
                                    // resuming a run whose coordinator workflow is gone
                                    // (cancelled, dismissed, never resumed), or its routing
                                    // key matched nothing. Replying Wait would make it poll
                                    // forever, leaving its run InProgress and the node
                                    // perpetually "busy" to invite/pre-flight checks. Return
                                    // a non-success status so the peer's bounded retry (3
                                    // strikes) gives up and finalizes the run. A coordinator
                                    // merely busy on a slow step keeps its registry entry and
                                    // still returns Wait via handle_command above, so this
                                    // fires only when no matching run exists.
                                    let mut resp = Response::new(Body::empty());
                                    *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                                    return Ok(resp);
                                }
                                (Some(_), None) => Message::new_empty(MessageType::Wait),
                            };
                            return Ok(Response::new(Body::from(resp.to_bytes())));
                        }
                        MessageType::InviteOnboarding
                        | MessageType::InviteKick
                        | MessageType::InviteContracts
                        | MessageType::InviteDars
                        | MessageType::InviteAddParty
                        | MessageType::InviteChangeThreshold => {
                            // Invites are accepted unconditionally: workflows are
                            // multi-instance now, so a node already coordinating or
                            // participating in runs can take part in more
                            // concurrently. Each invite surfaces as its own pending
                            // card (deduped by id, which carries the coordinator's
                            // run instance); the old refuse-while-busy gate was a
                            // one-workflow-at-a-time leftover.
                            let invitation_type = match msg.msg_type {
                                MessageType::InviteOnboarding => InvitationType::Onboarding,
                                MessageType::InviteKick => InvitationType::Kick,
                                MessageType::InviteContracts => InvitationType::Contracts,
                                MessageType::InviteDars => InvitationType::Dars,
                                MessageType::InviteAddParty => InvitationType::AddParty,
                                MessageType::InviteChangeThreshold => {
                                    InvitationType::ChangeThreshold
                                }
                                _ => unreachable!(),
                            };
                            tracing::info!(
                                "Received {invitation_type} invite, storing as pending invitation"
                            );
                            let meta = if msg.payload.is_empty() {
                                InvitationMeta::None
                            } else {
                                match invitation_type {
                                    InvitationType::Onboarding => {
                                        match serde_json::from_slice::<OnboardingInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::Onboarding(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Onboarding invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                    InvitationType::Dars => {
                                        match serde_json::from_slice::<DarsInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::Dars(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Dars invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                    InvitationType::Kick => {
                                        match serde_json::from_slice::<KickInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::Kick(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Kick invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                    InvitationType::Contracts => {
                                        match serde_json::from_slice::<ContractsInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::Contracts(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Contracts invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                    InvitationType::AddParty => {
                                        match serde_json::from_slice::<AddPartyInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::AddParty(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Add-party invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                    InvitationType::ChangeThreshold => {
                                        match serde_json::from_slice::<ChangeThresholdInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::ChangeThreshold(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Change-threshold invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                }
                            };
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                triggers
                                    .record_invitation(invitation_type, pubkey, meta)
                                    .await;
                            }

                            let ack = Message::new_empty(MessageType::Ack);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(ack.to_bytes()))
                                .unwrap());
                        }
                        MessageType::CancelInvite => {
                            // `msg.instance` scopes the cancel to one coordinator
                            // run; empty (legacy sender) cancels everything from
                            // the sender.
                            tracing::info!(
                                "Received CancelInvite (instance: {:?}), dropping pending \
                                 invites + cancelling matching in-flight peer runs from sender",
                                msg.instance
                            );
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                triggers.drop_invitations_from(pubkey, &msg.instance).await;
                                triggers.cancel_peer_runs_from(pubkey, &msg.instance).await;
                            }
                            let ack = Message::new_empty(MessageType::Ack);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(ack.to_bytes()))
                                .unwrap());
                        }
                        MessageType::RetryWorkflow => {
                            // `msg.instance` scopes the retry to one coordinator
                            // run; empty (legacy sender) retries every Failed run
                            // from the sender.
                            tracing::info!(
                                "Received RetryWorkflow (instance: {:?}), retrying matching \
                                 Failed peer runs from sender",
                                msg.instance
                            );
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                triggers.retry_peer_runs_from(pubkey, &msg.instance).await;
                            }
                            let ack = Message::new_empty(MessageType::Ack);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(ack.to_bytes()))
                                .unwrap());
                        }
                        _ => {
                            tracing::debug!("Ignoring message type {:?}", msg.msg_type);
                        }
                    }
                }

                Ok(Response::new(Body::empty()))
            }
        },
        // Must exceed the client's per-chunk timeout (NOISE_CHUNK_TIMEOUT) — this
        // bounds the whole response write, and a 1 MiB chunk can take well over
        // the old 5s on a CPU-constrained node, which truncated DAR transfers
        // mid-body ("end of file before message length reached").
        Some(NOISE_HANDLER_TIMEOUT),
    )
    .await;

    match result {
        Ok(()) => {
            tracing::debug!("Connection from {peer_addr} handled successfully");
        }
        Err(e) => {
            tracing::debug!("Noise connection from {peer_addr} failed: {e}");
        }
    }
}
