use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    sync::Arc,
    time::Instant,
};

use hyper::{Body, Request, Response, StatusCode};
use secp256k1::PublicKey;
use sqlx::SqlitePool;
use tokio::net::TcpListener;
use tokio_noise::handshakes::nn_psk2::Responder;

use crate::{
    config::{NetworkConfig, NodeConfig},
    db::schema::SchemaRead,
    noise::{
        CHUNK_SIZE, MAX_PAYLOAD_SIZE, Message, MessageType, NOISE_REQUEST_TIMEOUT, NoiseError,
        NoiseKeypair, parse_public_key,
    },
    participant_id::CantonId,
    server::{
        WorkflowProgress,
        peer_status::{LastSeen, bump},
    },
    workflow::{WorkflowState, state::WorkflowStep},
};

/// Whether an outgoing command should carry the workflow's `command_payload`.
///
/// `Disconnect` is a pure control signal marking the end of a workflow and
/// must never inherit the residual payload from an earlier step (e.g. the
/// DAR bundle from `UploadDars`). Shipping it would turn a small control
/// message into a multi-MB chunked transfer and delay the peer's invite
/// listener from resuming, which has caused Contracts-invite races in CI.
const fn command_carries_payload(command: MessageType) -> bool {
    !matches!(command, MessageType::Disconnect)
}

/// Coordinator server that accepts connections from peers
pub struct NoiseServer<S: WorkflowStep + 'static> {
    node_config: Arc<NodeConfig>,
    keypair: Arc<NoiseKeypair>,
    peer_keys: HashMap<String, PublicKey>,
    workflow_state: Arc<WorkflowState<S>>,
    last_seen: LastSeen,
    _p: PhantomData<S>,
}

impl<S: WorkflowStep + 'static> NoiseServer<S> {
    /// Create a new Noise server
    ///
    /// # Arguments
    /// * `node_config` - Node configuration
    /// * `network_config` - Network configuration
    /// * `db` - SQLite pool used to persist `WorkflowState` updates against the
    ///   matching `workflow_runs` row.
    /// * `instance_name` - Identifier for the persisted run (matches the row's
    ///   primary key).
    /// * `initial_step` - Initial workflow step
    /// * `exclude_participants` - Optional list of participant IDs to exclude from peers (e.g., participants being kicked)
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        node_config: NodeConfig,
        network_config: NetworkConfig,
        db: SqlitePool,
        instance_name: String,
        initial_step: S,
        exclude_participants: Option<Vec<String>>,
        last_seen: LastSeen,
    ) -> Result<Self, NoiseError> {
        let keypair = NoiseKeypair::from_file(&node_config.key_file_path()).await?;

        let excluded: HashSet<String> = exclude_participants
            .unwrap_or_default()
            .into_iter()
            .collect();

        let mut peer_keys = HashMap::new();
        for peer in &network_config.peers {
            let peer_id = peer.participant_id.to_string();
            if peer.participant_id == *node_config.participant_id() || excluded.contains(&peer_id) {
                continue;
            }

            let pub_key = parse_public_key(&peer.public_key)?;
            peer_keys.insert(peer_id, pub_key);
        }

        let expected_peers: Vec<CantonId> = network_config
            .peers
            .iter()
            .filter(|p| {
                let peer_id = p.participant_id.to_string();
                p.participant_id != *node_config.participant_id() && !excluded.contains(&peer_id)
            })
            .map(|p| p.participant_id.clone())
            .collect();

        if !excluded.is_empty() {
            tracing::info!(
                "Excluding {count} participant(s) from peers: {participants}",
                count = excluded.len(),
                participants = excluded.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        }

        tracing::info!(
            "Expected {count} peer(s): {peers}",
            count = expected_peers.len(),
            peers = expected_peers
                .iter()
                .map(CantonId::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Resume-aware construction: if an InProgress workflow_runs row already
        // exists for `instance_name` (we restarted mid-flight), re-hydrate the
        // state machine from its persisted `current_step` + already-completed
        // peers instead of starting fresh from `initial_step`. The
        // coordinator workflow loop is naturally driven by `current_step`, so
        // seeding it from the row picks the run back up at the right place.
        let workflow_state = match SchemaRead::get_workflow_run(&db, &instance_name).await {
            Ok(Some(run)) if run.status == WorkflowProgress::InProgress => {
                match S::try_from_step_name(&run.current_step) {
                    Some(step) => {
                        tracing::info!(
                            "Resuming workflow {instance_name} at step {step:?} \
                             ({} of {} peers completed)",
                            run.completed_peers.len(),
                            run.expected_peers.len()
                        );
                        WorkflowState::from_persisted(
                            db,
                            instance_name,
                            step,
                            expected_peers,
                            run.completed_peers,
                        )
                    }
                    None => {
                        tracing::warn!(
                            "Persisted current_step {:?} for {instance_name} is not a valid \
                             {kind} step; starting fresh from {initial_step:?}",
                            run.current_step,
                            kind = std::any::type_name::<S>()
                        );
                        WorkflowState::new(db, instance_name, initial_step, expected_peers)
                    }
                }
            }
            Ok(_) => WorkflowState::new(db, instance_name, initial_step, expected_peers),
            Err(e) => {
                tracing::warn!(
                    "Failed to look up persisted workflow_runs row for {instance_name}: {e}; \
                     starting fresh"
                );
                WorkflowState::new(db, instance_name, initial_step, expected_peers)
            }
        };

        Ok(Self {
            node_config: Arc::new(node_config),
            keypair: Arc::new(keypair),
            peer_keys,
            workflow_state,
            last_seen,
            _p: PhantomData,
        })
    }

    pub fn get_workflow_state(&self) -> Arc<WorkflowState<S>> {
        self.workflow_state.clone()
    }

    /// Start the server and listen for connections
    pub async fn start(self: Arc<Self>) -> Result<(), NoiseError> {
        let listen_addr = format!(
            "{host}:{port}",
            host = self.node_config.node.listen_address,
            port = self.node_config.node.port
        );

        tracing::info!("Starting Noise server on {listen_addr}");

        let listener = TcpListener::bind(&listen_addr)
            .await
            .map_err(NoiseError::Io)?;

        let make_responder = {
            let keypair = self.keypair.clone();
            let peer_keys = self.peer_keys.clone();

            move |_| {
                let secret_key = keypair.secret_key;
                let peer_keys = peer_keys.clone();

                // Create PSK derivation function
                Responder::new(move |identity: &[u8]| -> Option<[u8; 32]> {
                    // Identity is the participant_id
                    let peer_id = std::str::from_utf8(identity).ok()?;
                    let peer_pub_key = peer_keys.get(peer_id)?;

                    // Derive PSK using ECDH
                    let psk = secp256k1::ecdh::SharedSecret::new(peer_pub_key, &secret_key);
                    Some(psk.secret_bytes())
                })
            }
        };

        let make_handle_request = {
            let server = self.clone();

            move |_| {
                let server = server.clone();

                move |peer_id: &[u8], req: Request<Body>| {
                    let server = server.clone();
                    let peer_id = peer_id.to_vec(); // Clone peer_id to own it

                    async move {
                        let peer_id_str = std::str::from_utf8(&peer_id)
                            .map_err(|_| {
                                NoiseError::UnknownPeer("Invalid peer ID encoding".to_string())
                            })?
                            .to_string();

                        server.handle_request(peer_id_str, req).await
                    }
                }
            }
        };

        hyper_noise::server::accept_and_serve_http(
            listener,
            make_responder,
            make_handle_request,
            Some(NOISE_REQUEST_TIMEOUT),
        )
        .await?;

        Ok(())
    }

    /// Handle an incoming request from an peer
    async fn handle_request(
        &self,
        peer_id: String,
        req: Request<Body>,
    ) -> Result<Response<Body>, NoiseError> {
        tracing::debug!("Received request from peer: {peer_id}");

        {
            let now = Instant::now();
            let mut map = self.last_seen.write().await;
            bump(&mut map, peer_id.clone(), now);
        }

        // The Noise handshake delivers the peer's identity as a string of the
        // form `prefix::namespace_hex` (set by the client side via
        // `node_config.participant_id().to_string()`). Parse it back into a
        // typed `CantonId` once at the entry point so every downstream call
        // can stay typed.
        let peer_id = CantonId::parse(&peer_id)
            .map_err(|e| NoiseError::UnknownPeer(format!("Invalid peer id {peer_id}: {e}")))?;

        // Read request body
        let body_bytes = hyper::body::to_bytes(req.into_body()).await?;

        // Parse message
        let message = Message::from_bytes(&body_bytes).map_err(|_| NoiseError::InvalidMessage)?;

        tracing::debug!(
            "Received message type {:?} from {peer_id}",
            message.msg_type
        );

        // Route message based on type
        let response = match message.msg_type {
            MessageType::Ping => Message::new_empty(MessageType::Pong),
            MessageType::GetNextCommand => self.handle_get_next_command(peer_id).await?,
            MessageType::GetChunk => self.handle_get_chunk(message.payload).await?,
            MessageType::KeysUpload => {
                self.handle_peer_data(peer_id, message.payload, "keys upload")
                    .await?
            }
            MessageType::DnsSignature => {
                self.handle_peer_data(peer_id, message.payload, "DNS signature")
                    .await?
            }
            MessageType::P2pSignatures => {
                self.handle_peer_data(peer_id, message.payload, "P2P signatures")
                    .await?
            }
            MessageType::SubmissionSignatures => {
                self.handle_peer_data(peer_id, message.payload, "submission signatures")
                    .await?
            }
            MessageType::KickSignatures => {
                self.handle_peer_data(peer_id, message.payload, "kick signatures")
                    .await?
            }
            MessageType::StatusUpdate => {
                self.handle_status_update(peer_id, message.payload).await?
            }
            _ => {
                tracing::warn!("Unhandled message type: {:?}", message.msg_type);
                Message::new(MessageType::Error, b"Unsupported message type".to_vec())
            }
        };

        Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(response.to_bytes()))?)
    }

    /// Handle peer requesting next command
    async fn handle_get_next_command(&self, peer_id: CantonId) -> Result<Message, NoiseError> {
        // Mark peer as connected on first command poll
        self.workflow_state.peer_connected(peer_id.clone()).await;

        // Check if peer has already completed current step
        let has_completed = self.workflow_state.has_peer_completed(&peer_id).await;
        if has_completed {
            // Peer has completed current step, tell them to wait
            tracing::debug!("Sending Wait to {peer_id} (already completed current step)");
            return Ok(Message::new_empty(MessageType::Wait));
        }

        // Get current command from workflow state
        if let Some(command) = self.workflow_state.current_command().await {
            tracing::info!("Sending command {command:?} to {peer_id}");
            // Include payload only for commands that carry data (e.g. UploadDars,
            // SignDns). See `command_carries_payload` for why Disconnect is excluded.
            let payload = if command_carries_payload(command) {
                self.workflow_state.get_command_payload().await
            } else {
                Vec::new()
            };
            if payload.is_empty() {
                Ok(Message::new_empty(command))
            } else if payload.len() <= MAX_PAYLOAD_SIZE {
                // Small payload - send directly
                Ok(Message::new(command, payload))
            } else {
                // Large payload - use chunked transfer
                let total_size = payload.len() as u32;
                let chunk_count = payload.len().div_ceil(CHUNK_SIZE) as u32;
                tracing::info!(
                    "Payload too large ({total_size} bytes), using chunked transfer ({chunk_count} chunks)"
                );

                // Build ChunkedCommand payload: [command_type (2 bytes)] [total_size (4 bytes)] [chunk_count (4 bytes)]
                let mut meta = Vec::with_capacity(10);
                meta.extend_from_slice(&command.to_u16().to_be_bytes());
                meta.extend_from_slice(&total_size.to_be_bytes());
                meta.extend_from_slice(&chunk_count.to_be_bytes());

                Ok(Message::new(MessageType::ChunkedCommand, meta))
            }
        } else {
            // No command for peers right now (coordinator-only step)
            tracing::debug!("Sending Wait to {peer_id} (coordinator-only step)");
            Ok(Message::new_empty(MessageType::Wait))
        }
    }

    /// Handle chunk request from peer
    async fn handle_get_chunk(&self, request_payload: Vec<u8>) -> Result<Message, NoiseError> {
        if request_payload.len() < 4 {
            return Err(NoiseError::InvalidMessage);
        }

        let chunk_index = u32::from_be_bytes([
            request_payload[0],
            request_payload[1],
            request_payload[2],
            request_payload[3],
        ]) as usize;

        let payload = self.workflow_state.get_command_payload().await;
        let start = chunk_index * CHUNK_SIZE;

        if start >= payload.len() {
            return Err(NoiseError::InvalidMessage);
        }

        let end = std::cmp::min(start + CHUNK_SIZE, payload.len());
        let chunk_data = &payload[start..end];

        tracing::debug!(
            "Sending chunk {chunk_index} ({start}..{end}, {} bytes)",
            chunk_data.len()
        );

        // Build Chunk response: [chunk_index (4 bytes)] [chunk_data (variable)]
        let mut response = Vec::with_capacity(4 + chunk_data.len());
        response.extend_from_slice(&(chunk_index as u32).to_be_bytes());
        response.extend_from_slice(chunk_data);

        Ok(Message::new(MessageType::Chunk, response))
    }

    /// Handle peer data upload (keys, signatures, etc.)
    async fn handle_peer_data(
        &self,
        peer_id: CantonId,
        payload: Vec<u8>,
        data_type: &str,
    ) -> Result<Message, NoiseError> {
        tracing::info!("Handling {data_type} from {peer_id}");

        self.workflow_state
            .store_peer_data(peer_id.clone(), payload)
            .await;

        self.workflow_state.peer_completed(peer_id).await;

        Ok(Message::new_empty(MessageType::Ack))
    }

    /// Handle status update from peer
    async fn handle_status_update(
        &self,
        peer_id: CantonId,
        payload: Vec<u8>,
    ) -> Result<Message, NoiseError> {
        let status = String::from_utf8_lossy(&payload);
        tracing::info!("Handling status update from {peer_id}: {status}");

        // Mark peer as completed for this step
        self.workflow_state.peer_completed(peer_id).await;

        Ok(Message::new_empty(MessageType::Ack))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_never_carries_a_payload() {
        assert!(!command_carries_payload(MessageType::Disconnect));
    }

    #[test]
    fn data_carrying_commands_preserve_their_payload() {
        assert!(command_carries_payload(MessageType::UploadDars));
        assert!(command_carries_payload(MessageType::SignDns));
        assert!(command_carries_payload(MessageType::SignP2p));
        assert!(command_carries_payload(MessageType::SignSubmissions));
        assert!(command_carries_payload(MessageType::SignKick));
        assert!(command_carries_payload(MessageType::GenerateKeys));
    }
}
