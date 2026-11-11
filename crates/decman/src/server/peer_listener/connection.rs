//! One inbound peer connection: every message type the listener answers.

use std::{net::SocketAddr, sync::Arc, time::Instant};

use hyper::{Body, Response, StatusCode};
use sqlx::SqlitePool;
use tokio::net::TcpStream;
use tokio_noise::handshakes::nn_psk2::Responder;

use super::{
    heartbeat::build_peer_key_map,
    replies::{dar_for_invited_peer, peer_payload_response},
    triggers::{InvitationMeta, WorkflowTriggers},
};
use crate::{
    canton_id::CantonId,
    db::schema::SchemaRead,
    noise::{CHUNK_SIZE, Message, MessageType, NOISE_HANDLER_TIMEOUT, NoiseKeypair},
    server::{
        WorkflowRegistry, health,
        owner_keys::list_my_owner_keys,
        peer_status,
        peer_status::LastSeen,
        state::PEER_CHUNK_CACHE_TTL,
        types::{
            AddPartyInvitePayload, ChangeThresholdInvitePayload, ContractsInvitePayload,
            DarsInvitePayload, InvitationType, KickInvitePayload, OnboardingInvitePayload,
        },
    },
};

/// Handle an incoming Noise connection (either ping or invite)
pub(crate) async fn handle_incoming_connection(
    socket: TcpStream,
    peer_addr: SocketAddr,
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
            let request = PeerRequest {
                sender: peer_id_str,
                sender_key: peer_pubkey_hex,
                triggers,
            };

            async move {
                // Bump last_seen for known peers.
                if let Some(id) = request.sender.as_deref()
                    && peer_keys.contains_key(id)
                {
                    let now = Instant::now();
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
                            sender = request.sender.as_deref().unwrap_or("<unidentified peer>")
                        );
                        // Answer WITH the reason rather than an empty body.
                        // An empty 503 here is indistinguishable at the
                        // requester from a proxy with no healthy backend, so
                        // the operator on the other side saw only "message too
                        // short" (#332). Status stays 503, so an older
                        // requester behaves exactly as before.
                        return Ok(unavailable(format!("request rejected: {e}").into_bytes()));
                    }
                };

                tracing::debug!("Received message type {:?}", msg.msg_type);

                let response = match msg.msg_type {
                    MessageType::Ping => answer_ping(our_pubkey),
                    MessageType::Health => answer_health(&request.triggers).await,
                    MessageType::RequestDar => serve_dar(&request, &msg).await,
                    MessageType::GetChunk => serve_chunk(&request, msg).await,
                    MessageType::RequestOwnerKeys => {
                        answer_owner_keys(&request.triggers, &msg).await
                    }
                    MessageType::ListPeers => answer_peer_list(&request.triggers).await,
                    MessageType::RequestMemberParty => {
                        answer_member_party(&request.triggers, &msg).await
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
                    | MessageType::DeclineInvitation => route_command(&request, msg).await,
                    MessageType::InviteOnboarding
                    | MessageType::InviteKick
                    | MessageType::InviteContracts
                    | MessageType::InviteDars
                    | MessageType::InviteAddParty
                    | MessageType::InviteChangeThreshold => record_invite(&request, &msg).await,
                    MessageType::CancelInvite => cancel_invites(&request, &msg).await,
                    MessageType::RetryWorkflow => retry_runs(&request, &msg).await,
                    _ => {
                        tracing::debug!("Ignoring message type {:?}", msg.msg_type);
                        Response::new(Body::empty())
                    }
                };
                Ok(response)
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

/// Who sent a peer message, and the node state its answer reads.
struct PeerRequest {
    /// The sender's participant id, when its identity is valid UTF-8.
    sender: Option<String>,
    /// The sender's Noise public key (hex), when the sender is a known peer.
    sender_key: Option<String>,
    triggers: WorkflowTriggers,
}

/// Serve a DAR of a Dars run to a peer that run invited: inline, or cached
/// for a chunked transfer.
async fn serve_dar(request: &PeerRequest, msg: &Message) -> Response<Body> {
    tracing::debug!("Received RequestDar request");
    let sender = request.sender.as_deref();
    let dar = match dar_for_invited_peer(&request.triggers.db, sender, &msg.payload).await {
        Ok(bytes) => bytes,
        Err(e) => {
            // Refusals are the interesting case here: a
            // peer asking for a DAR of a run it was not
            // invited to is worth seeing in the log.
            tracing::warn!(
                "Refused a DAR to {}: {e:#}",
                sender.unwrap_or("<unidentified peer>")
            );
            let response_msg = Message::new(MessageType::Error, format!("{e:#}").into_bytes());
            return reply(response_msg);
        }
    };
    peer_payload_response(
        dar,
        request.sender_key.as_deref(),
        &request.triggers.peer_chunk_cache,
        "Requested DAR",
    )
    .await
}

/// Serve one chunk: of a coordinator run's chunked command when
/// `msg.instance` names the run, otherwise of the chunked DAR cached for
/// this peer.
async fn serve_chunk(request: &PeerRequest, msg: Message) -> Response<Body> {
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
        return match route_to_run(&request.triggers.workflows, request.sender.as_deref(), msg).await
        {
            Some(resp) => reply(resp),
            // The named run is gone (cancelled/finished) —
            // 503 so the peer's bounded retry gives up.
            // Says so in the body, so the requester
            // reports the cause rather than an empty reply.
            None => unavailable(
                b"no such workflow run on this peer: it was \
                  cancelled or already finished"
                    .to_vec(),
            ),
        };
    }
    if msg.payload.len() < 4 {
        tracing::warn!("Received GetChunk request with payload < 4 bytes");
        return empty_reply(StatusCode::BAD_REQUEST);
    }
    let chunk_index = u32::from_be_bytes([
        msg.payload[0],
        msg.payload[1],
        msg.payload[2],
        msg.payload[3],
    ]) as usize;

    let Some(ref pk) = request.sender_key else {
        tracing::warn!("Received GetChunk request without identifiable peer pubkey");
        return empty_reply(StatusCode::BAD_REQUEST);
    };

    let chunk_bytes = {
        let mut cache = request.triggers.peer_chunk_cache.lock().await;

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
                return empty_reply(StatusCode::NOT_FOUND);
            }
            Some(true) => {
                // Expired — drop the stale entry now so it doesn't
                // linger until the next RequestDar-driven `retain`.
                cache.remove(pk);
                tracing::warn!(
                    "GetChunk for {pk} chunk {chunk_index}: cache entry \
                     expired (removed)"
                );
                return empty_reply(StatusCode::NOT_FOUND);
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
            return empty_reply(StatusCode::BAD_REQUEST);
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
    reply(response_msg)
}

/// Route workflow-command traffic to the coordinator run identified by
/// `msg.instance` (the peer stamps its coordinator's run name).
async fn route_command(request: &PeerRequest, msg: Message) -> Response<Body> {
    match route_to_run(&request.triggers.workflows, request.sender.as_deref(), msg).await {
        Some(resp) => reply(resp),
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
        None => empty_reply(StatusCode::SERVICE_UNAVAILABLE),
    }
}

/// Answer a ping with this node's Noise public key.
fn answer_ping(our_pubkey: String) -> Response<Body> {
    tracing::debug!("Received ping, responding with pong");
    let pong = Message::new(MessageType::Pong, our_pubkey.into_bytes());
    reply(pong)
}

/// Answer a health probe.
async fn answer_health(triggers: &WorkflowTriggers) -> Response<Body> {
    // Always answer health — even mid-workflow. Reports
    // liveness plus this node's in-flight workflow (if
    // any) so peers don't infer "offline" from a busy node.
    let resp =
        health::build_health_response(&triggers.db, &triggers.config.participant_id().to_string())
            .await;
    let msg = Message::new(MessageType::HealthResponse, resp.to_payload());
    reply(msg)
}

/// Answer with this node's owner keys of the parties `msg` lists.
async fn answer_owner_keys(triggers: &WorkflowTriggers, msg: &Message) -> Response<Body> {
    tracing::debug!("Received RequestOwnerKeys request");
    // Payload is a JSON array of decentralized party_ids
    // the caller wants this node's owner_keys for (see
    // #149: peer no longer enumerates the synchronizer).
    // A malformed or absent payload is treated as
    // "nothing requested" — return an empty list rather
    // than fall back to a slow whole-synchronizer scan.
    let requested_party_ids: Vec<String> = serde_json::from_slice(&msg.payload).unwrap_or_default();
    let payload = match list_my_owner_keys(&triggers.config, &requested_party_ids).await {
        Ok(data) => data,
        Err(e) => {
            tracing::error!("Failed to list owner keys: {e}");
            b"[]".to_vec()
        }
    };
    let response_msg = Message::new(MessageType::OwnerKeys, payload);
    reply(response_msg)
}

/// Answer with the participant ids of this node's peers.
async fn answer_peer_list(triggers: &WorkflowTriggers) -> Response<Body> {
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
    reply(response_msg)
}

/// Answer with this node's member party of the decentralized party `msg`
/// names, or nothing when it has none.
async fn answer_member_party(triggers: &WorkflowTriggers, msg: &Message) -> Response<Body> {
    let dec_party_id = std::str::from_utf8(&msg.payload).unwrap_or("").to_string();
    tracing::debug!("Received RequestMemberParty for {dec_party_id}",);
    let payload = {
        let creds = triggers.party_credentials.read().await;
        creds
            .iter()
            .find(|c| c.dec_party_id.to_string() == dec_party_id)
            .map(|c| c.member_party_id.to_string().into_bytes())
            .unwrap_or_default()
    };
    let response_msg = Message::new(MessageType::MemberPartyResponse, payload);
    reply(response_msg)
}

/// Record an invite as a pending invitation, and acknowledge it.
async fn record_invite(request: &PeerRequest, msg: &Message) -> Response<Body> {
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
        MessageType::InviteChangeThreshold => InvitationType::ChangeThreshold,
        _ => unreachable!(),
    };
    tracing::info!("Received {invitation_type} invite, storing as pending invitation");
    let meta = invitation_meta(invitation_type, &msg.payload);
    if let Some(ref pubkey) = request.sender_key {
        request
            .triggers
            .record_invitation(invitation_type, pubkey, meta)
            .await;
    }

    ack()
}

/// The details an invite's payload carries for its pending invitation. A
/// payload that does not parse is logged and carries none.
fn invitation_meta(invitation_type: InvitationType, payload: &[u8]) -> InvitationMeta {
    if payload.is_empty() {
        InvitationMeta::None
    } else {
        match invitation_type {
            InvitationType::Onboarding => {
                match serde_json::from_slice::<OnboardingInvitePayload>(payload) {
                    Ok(p) => InvitationMeta::Onboarding(p),
                    Err(e) => {
                        tracing::warn!("Onboarding invite payload was unparseable: {e}");
                        InvitationMeta::None
                    }
                }
            }
            InvitationType::Dars => match serde_json::from_slice::<DarsInvitePayload>(payload) {
                Ok(p) => InvitationMeta::Dars(p),
                Err(e) => {
                    tracing::warn!("Dars invite payload was unparseable: {e}");
                    InvitationMeta::None
                }
            },
            InvitationType::Kick => match serde_json::from_slice::<KickInvitePayload>(payload) {
                Ok(p) => InvitationMeta::Kick(p),
                Err(e) => {
                    tracing::warn!("Kick invite payload was unparseable: {e}");
                    InvitationMeta::None
                }
            },
            InvitationType::Contracts => {
                match serde_json::from_slice::<ContractsInvitePayload>(payload) {
                    Ok(p) => InvitationMeta::Contracts(p),
                    Err(e) => {
                        tracing::warn!("Contracts invite payload was unparseable: {e}");
                        InvitationMeta::None
                    }
                }
            }
            InvitationType::AddParty => {
                match serde_json::from_slice::<AddPartyInvitePayload>(payload) {
                    Ok(p) => InvitationMeta::AddParty(p),
                    Err(e) => {
                        tracing::warn!("Add-party invite payload was unparseable: {e}");
                        InvitationMeta::None
                    }
                }
            }
            InvitationType::ChangeThreshold => {
                match serde_json::from_slice::<ChangeThresholdInvitePayload>(payload) {
                    Ok(p) => InvitationMeta::ChangeThreshold(p),
                    Err(e) => {
                        tracing::warn!("Change-threshold invite payload was unparseable: {e}");
                        InvitationMeta::None
                    }
                }
            }
        }
    }
}

/// Drop the sender's pending invites and cancel its peer runs that match
/// `msg.instance`, and acknowledge.
async fn cancel_invites(request: &PeerRequest, msg: &Message) -> Response<Body> {
    // `msg.instance` scopes the cancel to one coordinator
    // run; empty (legacy sender) cancels everything from
    // the sender.
    tracing::info!(
        "Received CancelInvite (instance: {:?}), dropping pending \
         invites + cancelling matching in-flight peer runs from sender",
        msg.instance
    );
    if let Some(ref pubkey) = request.sender_key {
        request
            .triggers
            .drop_invitations_from(pubkey, &msg.instance)
            .await;
        request
            .triggers
            .cancel_peer_runs_from(pubkey, &msg.instance)
            .await;
    }
    ack()
}

/// Retry the sender's failed peer runs that match `msg.instance`, and
/// acknowledge.
async fn retry_runs(request: &PeerRequest, msg: &Message) -> Response<Body> {
    // `msg.instance` scopes the retry to one coordinator
    // run; empty (legacy sender) retries every Failed run
    // from the sender.
    tracing::info!(
        "Received RetryWorkflow (instance: {:?}), retrying matching \
         Failed peer runs from sender",
        msg.instance
    );
    if let Some(ref pubkey) = request.sender_key {
        request
            .triggers
            .retry_peer_runs_from(pubkey, &msg.instance)
            .await;
    }
    ack()
}

/// A reply that carries `message`, with status 200.
fn reply(message: Message) -> Response<Body> {
    Response::new(Body::from(message.to_bytes()))
}

/// A reply with `status` and no body.
fn empty_reply(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

/// A 503 whose body is an `Error` message with `text`. An empty 503 is
/// indistinguishable at the requester from a proxy with no healthy
/// backend, so the text says why.
fn unavailable(text: Vec<u8>) -> Response<Body> {
    let mut response = reply(Message::new(MessageType::Error, text));
    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    response
}

/// The acknowledgement of a message that needs no other answer.
fn ack() -> Response<Body> {
    reply(Message::new_empty(MessageType::Ack))
}

/// Route a workflow message to the coordinator run that `msg.instance`
/// names, and return that run's answer. `None` when no such run exists on
/// this node. A sender whose id does not parse is told to wait. The
/// registry lock is held only long enough to clone the handle out — never
/// across the await.
async fn route_to_run(
    workflows: &WorkflowRegistry,
    sender: Option<&str>,
    msg: Message,
) -> Option<Message> {
    let active = workflows.route(&msg.instance);
    let peer = sender.and_then(|s| CantonId::parse(s).ok());
    match (active, peer) {
        (Some(wf), Some(pid)) => Some(
            wf.handle_command(pid, msg)
                .await
                .unwrap_or_else(|e| Message::new(MessageType::Error, format!("{e}").into_bytes())),
        ),
        (None, _) => None,
        (Some(_), None) => Some(Message::new_empty(MessageType::Wait)),
    }
}

#[cfg(test)]
mod reply_tests {
    use hyper::body::to_bytes;

    use super::*;

    async fn parts(response: Response<Body>) -> anyhow::Result<(StatusCode, Vec<u8>)> {
        let status = response.status();
        Ok((status, to_bytes(response.into_body()).await?.to_vec()))
    }

    /// Each reply kind keeps the status and the bytes the arms built by
    /// hand.
    #[tokio::test]
    async fn each_reply_kind_keeps_its_status_and_bytes() -> anyhow::Result<()> {
        let pong = Message::new(MessageType::Pong, b"key".to_vec());
        assert_eq!(
            parts(reply(pong.clone())).await?,
            (StatusCode::OK, pong.to_bytes())
        );
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::NOT_FOUND,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert_eq!(parts(empty_reply(status)).await?, (status, Vec::new()));
        }
        assert_eq!(
            parts(unavailable(b"gone".to_vec())).await?,
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Message::new(MessageType::Error, b"gone".to_vec()).to_bytes()
            )
        );
        assert_eq!(
            parts(ack()).await?,
            (
                StatusCode::OK,
                Message::new_empty(MessageType::Ack).to_bytes()
            )
        );
        for response in [
            reply(pong.clone()),
            empty_reply(StatusCode::NOT_FOUND),
            unavailable(Vec::new()),
            ack(),
        ] {
            assert_eq!(response.version(), hyper::Version::HTTP_11);
            assert!(response.headers().is_empty());
        }
        Ok(())
    }

    /// A message for a run this node does not have gets no answer from a
    /// run, whoever sends it.
    #[tokio::test]
    async fn a_message_for_an_unknown_run_is_not_routed() {
        let workflows = WorkflowRegistry::new();
        let msg = Message::new(MessageType::GetNextCommand, Vec::new()).with_instance("gone");
        assert!(route_to_run(&workflows, None, msg.clone()).await.is_none());
        assert!(
            route_to_run(&workflows, Some("not a canton id"), msg)
                .await
                .is_none()
        );
    }
}

#[cfg(test)]
mod workflow_message_tests {
    //! Calls each workflow-traffic message function directly.

    use std::{collections::HashMap, sync::Arc, time::Duration};

    use hyper::body::to_bytes;
    use tokio::sync::{Mutex, RwLock, mpsc};

    use super::*;
    use crate::{config::NodeConfig, db::MIGRATOR, server::types::RequestDarPayload};

    type Result<T = ()> = anyhow::Result<T>;

    const KEY: &str = "02abcdef";

    fn request(db: SqlitePool, sender_key: Option<&str>) -> PeerRequest {
        PeerRequest {
            sender: Some(format!("validator-2::1220{}", "b".repeat(64))),
            sender_key: sender_key.map(str::to_string),
            triggers: WorkflowTriggers {
                pending_invitations: Arc::new(RwLock::new(Vec::new())),
                config: NodeConfig::default(),
                peer_chunk_cache: Arc::new(Mutex::new(HashMap::new())),
                db,
                party_credentials: Arc::new(RwLock::new(Vec::new())),
                workflows: WorkflowRegistry::new(),
                peer_job_sender: mpsc::unbounded_channel().0,
            },
        }
    }

    async fn parts(response: Response<Body>) -> Result<(StatusCode, Vec<u8>)> {
        let status = response.status();
        Ok((status, to_bytes(response.into_body()).await?.to_vec()))
    }

    fn get_chunk(index: u32) -> Message {
        Message::new(MessageType::GetChunk, index.to_be_bytes().to_vec())
    }

    /// Cache `payload` for the sender, read `age` ago.
    async fn cache(request: &PeerRequest, payload: &[u8], age: Duration) {
        request
            .triggers
            .peer_chunk_cache
            .lock()
            .await
            .insert(KEY.to_string(), (payload.to_vec(), Instant::now() - age));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_command_for_an_unknown_run_is_a_bare_503(pool: SqlitePool) -> Result {
        let msg = Message::new(MessageType::GetNextCommand, Vec::new()).with_instance("gone");
        assert_eq!(
            parts(route_command(&request(pool, Some(KEY)), msg).await).await?,
            (StatusCode::SERVICE_UNAVAILABLE, Vec::new())
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_chunk_of_an_unknown_run_says_the_run_is_gone(pool: SqlitePool) -> Result {
        let msg = get_chunk(0).with_instance("gone");
        let expected = Message::new(
            MessageType::Error,
            b"no such workflow run on this peer: it was cancelled or already finished".to_vec(),
        );
        assert_eq!(
            parts(serve_chunk(&request(pool, Some(KEY)), msg).await).await?,
            (StatusCode::SERVICE_UNAVAILABLE, expected.to_bytes())
        );
        Ok(())
    }

    /// The chunk cache answers each bad request with a bare status, and a
    /// good one with the chunk and its index.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_chunk_cache_answers(pool: SqlitePool) -> Result {
        let short = Message::new(MessageType::GetChunk, vec![0, 0]);
        let request_with_key = request(pool.clone(), Some(KEY));
        assert_eq!(
            parts(serve_chunk(&request_with_key, short).await).await?,
            (StatusCode::BAD_REQUEST, Vec::new())
        );
        assert_eq!(
            parts(serve_chunk(&request(pool, None), get_chunk(0)).await).await?,
            (StatusCode::BAD_REQUEST, Vec::new())
        );
        assert_eq!(
            parts(serve_chunk(&request_with_key, get_chunk(0)).await).await?,
            (StatusCode::NOT_FOUND, Vec::new())
        );

        let payload = vec![7u8; CHUNK_SIZE + 3];
        cache(&request_with_key, &payload, Duration::ZERO).await;
        let mut expected = 1u32.to_be_bytes().to_vec();
        expected.extend_from_slice(&[7, 7, 7]);
        assert_eq!(
            parts(serve_chunk(&request_with_key, get_chunk(1)).await).await?,
            (
                StatusCode::OK,
                Message::new(MessageType::Chunk, expected).to_bytes()
            )
        );
        assert_eq!(
            parts(serve_chunk(&request_with_key, get_chunk(2)).await).await?,
            (StatusCode::BAD_REQUEST, Vec::new())
        );

        cache(&request_with_key, &payload, PEER_CHUNK_CACHE_TTL).await;
        assert_eq!(
            parts(serve_chunk(&request_with_key, get_chunk(0)).await).await?,
            (StatusCode::NOT_FOUND, Vec::new())
        );
        assert!(
            request_with_key
                .triggers
                .peer_chunk_cache
                .lock()
                .await
                .is_empty(),
            "an expired entry is removed"
        );
        Ok(())
    }

    /// A DAR request for a run that does not exist answers 200 with the
    /// refusal in an `Error` message.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_refused_dar_says_why(pool: SqlitePool) -> Result {
        let request = request(pool, Some(KEY));
        let payload = serde_json::to_vec(&RequestDarPayload {
            workflow_instance: "dars-1".to_string(),
            index: 0,
            filename: "app.dar".to_string(),
        })?;
        let msg = Message::new(MessageType::RequestDar, payload.clone());
        let refusal =
            dar_for_invited_peer(&request.triggers.db, request.sender.as_deref(), &payload)
                .await
                .err()
                .ok_or_else(|| anyhow::anyhow!("there is no run to serve"))?;
        assert_eq!(
            parts(serve_dar(&request, &msg).await).await?,
            (
                StatusCode::OK,
                Message::new(MessageType::Error, format!("{refusal:#}").into_bytes()).to_bytes()
            )
        );
        Ok(())
    }
}

#[cfg(test)]
mod status_and_invite_message_tests {
    //! Calls each status and invitation message function directly.

    use std::{collections::HashMap, sync::Arc};

    use hyper::body::to_bytes;
    use serde_json::json;
    use tokio::sync::{Mutex, RwLock, mpsc};

    use super::*;
    use crate::{
        config::{KeycloakConfig, NodeConfig, PackageConfig, PartyCredentials, Peer},
        db::{
            MIGRATOR,
            schema::{Commitable, SchemaWrite},
        },
    };

    type Result<T = ()> = anyhow::Result<T>;

    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn cid(prefix: &str) -> Result<CantonId> {
        CantonId::parse(&format!("{prefix}::1220{}", "a".repeat(64)))
    }

    /// A message from `validator-2` to this node, `p1`.
    fn request(db: SqlitePool, sender_key: Option<&str>) -> PeerRequest {
        let mut config = NodeConfig::default();
        config.node.participant_id = cid("p1").ok();
        PeerRequest {
            sender: Some("validator-2".to_string()),
            sender_key: sender_key.map(str::to_string),
            triggers: WorkflowTriggers {
                pending_invitations: Arc::new(RwLock::new(Vec::new())),
                config,
                peer_chunk_cache: Arc::new(Mutex::new(HashMap::new())),
                db,
                party_credentials: Arc::new(RwLock::new(Vec::new())),
                workflows: WorkflowRegistry::new(),
                peer_job_sender: mpsc::unbounded_channel().0,
            },
        }
    }

    /// The reply's status and the message it carries.
    async fn answer(response: Response<Body>) -> Result<(StatusCode, Message)> {
        let status = response.status();
        let bytes = to_bytes(response.into_body()).await?;
        Ok((status, Message::from_bytes(&bytes)?))
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_status_messages_answer(pool: SqlitePool) -> Result {
        let (status, pong) = answer(answer_ping("02key".to_string())).await?;
        assert_eq!((status, pong.msg_type), (StatusCode::OK, MessageType::Pong));
        assert_eq!(pong.payload, b"02key");

        let peer = Peer {
            participant_id: cid("p2")?,
            name: "p2".to_string(),
            address: "127.0.0.1".to_string(),
            port: 1,
            public_key: String::new(),
            party: None,
        };
        let mut tx = pool.begin_transaction().await?;
        tx.insert_peer(&peer).await?;
        Commitable::commit(tx).await?;
        let request = request(pool, Some(KEY));

        let (status, health) = answer(answer_health(&request.triggers).await).await?;
        assert_eq!(
            (status, health.msg_type),
            (StatusCode::OK, MessageType::HealthResponse)
        );

        let (status, peers) = answer(answer_peer_list(&request.triggers).await).await?;
        assert_eq!(
            (status, peers.msg_type),
            (StatusCode::OK, MessageType::PeerList)
        );
        assert_eq!(
            peers.payload,
            serde_json::to_vec(&[cid("p2")?.to_string()])?
        );

        // A payload that names no party asks for nothing, so no Canton call.
        let ask = Message::new(MessageType::RequestOwnerKeys, b"not json".to_vec());
        let (status, keys) = answer(answer_owner_keys(&request.triggers, &ask).await).await?;
        assert_eq!(
            (status, keys.msg_type),
            (StatusCode::OK, MessageType::OwnerKeys)
        );
        assert_eq!(keys.payload, b"[]");
        Ok(())
    }

    /// A member party is answered for a party this node has credentials
    /// for, and an empty payload for any other.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_member_party_comes_from_the_credentials(pool: SqlitePool) -> Result {
        let request = request(pool, Some(KEY));
        request
            .triggers
            .party_credentials
            .write()
            .await
            .push(PartyCredentials {
                dec_party_id: cid("dec")?,
                member_party_id: cid("member")?,
                user_id: "user".to_string(),
                keycloak: KeycloakConfig::default(),
                auth0: None,
                packages: PackageConfig::default(),
            });
        let ask =
            |party: &str| Message::new(MessageType::RequestMemberParty, party.as_bytes().to_vec());

        let known = ask(&cid("dec")?.to_string());
        let (status, member) = answer(answer_member_party(&request.triggers, &known).await).await?;
        assert_eq!(
            (status, member.msg_type),
            (StatusCode::OK, MessageType::MemberPartyResponse)
        );
        assert_eq!(member.payload, cid("member")?.to_string().into_bytes());

        let unknown = ask(&cid("other")?.to_string());
        let (_, member) = answer(answer_member_party(&request.triggers, &unknown).await).await?;
        assert!(member.payload.is_empty());
        Ok(())
    }

    /// An invite from a known sender is recorded and acknowledged, and a
    /// cancel from that sender drops it again. A sender without a key is
    /// acknowledged and changes nothing.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn invites_are_recorded_and_cancelled(pool: SqlitePool) -> Result {
        let invite = Message::new(
            MessageType::InviteOnboarding,
            serde_json::to_vec(&json!({
                "prefix": "newparty",
                "participants": [cid("p2")?.to_string()],
                "workflow_instance": "newparty-creation",
            }))?,
        );
        let unknown = request(pool.clone(), None);
        let (status, ack) = answer(record_invite(&unknown, &invite).await).await?;
        assert_eq!((status, ack.msg_type), (StatusCode::OK, MessageType::Ack));
        assert!(unknown.triggers.pending_invitations.read().await.is_empty());

        let known = request(pool, Some(KEY));
        let (_, ack) = answer(record_invite(&known, &invite).await).await?;
        assert_eq!(ack.msg_type, MessageType::Ack);
        {
            let pending = known.triggers.pending_invitations.read().await;
            let [invitation] = pending.as_slice() else {
                anyhow::bail!("one invitation must be pending: {pending:?}");
            };
            assert_eq!(invitation.coordinator_pubkey, KEY);
            assert_eq!(invitation.prefix.as_deref(), Some("newparty"));
        }

        let retry =
            Message::new_empty(MessageType::RetryWorkflow).with_instance("newparty-creation");
        let (_, ack) = answer(retry_runs(&known, &retry).await).await?;
        assert_eq!(ack.msg_type, MessageType::Ack);

        let cancel =
            Message::new_empty(MessageType::CancelInvite).with_instance("newparty-creation");
        let (_, ack) = answer(cancel_invites(&known, &cancel).await).await?;
        assert_eq!(ack.msg_type, MessageType::Ack);
        assert!(known.triggers.pending_invitations.read().await.is_empty());
        Ok(())
    }

    /// A payload that does not parse carries no details, and an empty one
    /// carries none either.
    #[test]
    fn an_unreadable_invite_payload_carries_nothing() {
        assert!(matches!(
            invitation_meta(InvitationType::Kick, b"{"),
            InvitationMeta::None
        ));
        assert!(matches!(
            invitation_meta(InvitationType::Dars, b""),
            InvitationMeta::None
        ));
    }
}
