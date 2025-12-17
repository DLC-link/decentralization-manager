use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    sync::Arc,
};

use hyper::{Body, Request, Response, StatusCode};
use secp256k1::PublicKey;
use tokio::net::TcpListener;
use tokio_noise::handshakes::nn_psk2::Responder;

use crate::{
    config::{NetworkConfig, NodeConfig},
    noise::{
        CHUNK_SIZE, MAX_PAYLOAD_SIZE, Message, MessageType, NOISE_REQUEST_TIMEOUT, NoiseError,
        NoiseKeypair, parse_public_key,
    },
    workflow::{WorkflowState, state::WorkflowStep},
};

/// Coordinator server that accepts connections from attestors
pub struct NoiseServer<S: WorkflowStep + 'static> {
    node_config: Arc<NodeConfig>,
    network_config: Arc<NetworkConfig>,
    keypair: Arc<NoiseKeypair>,
    peer_keys: HashMap<String, PublicKey>,
    workflow_state: Arc<WorkflowState<S>>,
    _p: PhantomData<S>,
}

impl<S: WorkflowStep + 'static> NoiseServer<S> {
    /// Create a new Noise server
    ///
    /// # Arguments
    /// * `node_config` - Node configuration
    /// * `network_config` - Network configuration
    /// * `initial_step` - Initial workflow step
    /// * `exclude_participants` - Optional list of participant IDs to exclude from attestors (e.g., participants being kicked)
    pub async fn new(
        node_config: NodeConfig,
        network_config: NetworkConfig,
        initial_step: S,
        exclude_participants: Option<Vec<String>>,
    ) -> Result<Self, NoiseError> {
        let keypair = NoiseKeypair::from_file(&node_config.key_file_path()).await?;

        let excluded: HashSet<String> = exclude_participants
            .unwrap_or_default()
            .into_iter()
            .collect();

        let mut peer_keys = HashMap::new();
        for peer in &network_config.peers {
            if peer.id == node_config.node.node_id || excluded.contains(&peer.id) {
                continue;
            }

            let pub_key = parse_public_key(&peer.public_key)?;
            peer_keys.insert(peer.id.clone(), pub_key);
        }

        let expected_attestors: Vec<String> = network_config
            .peers
            .iter()
            .filter(|p| p.id != node_config.node.node_id && !excluded.contains(&p.id))
            .map(|p| p.id.clone())
            .collect();

        if !excluded.is_empty() {
            tracing::info!(
                "Excluding {count} participant(s) from attestors: {participants}",
                count = excluded.len(),
                participants = excluded.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        }

        tracing::info!(
            "Expected {count} attestor(s): {attestors}",
            count = expected_attestors.len(),
            attestors = expected_attestors.join(", ")
        );

        let workflow_state = WorkflowState::new(initial_step, expected_attestors);

        Ok(Self {
            node_config: Arc::new(node_config),
            network_config: Arc::new(network_config),
            keypair: Arc::new(keypair),
            peer_keys,
            workflow_state,
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
            port = self
                .network_config
                .get_peer(&self.node_config.node.node_id)
                .ok_or_else(|| NoiseError::UnknownPeer(self.node_config.node.node_id.clone()))?
                .port
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

    /// Handle an incoming request from an attestor
    async fn handle_request(
        &self,
        peer_id: String,
        req: Request<Body>,
    ) -> Result<Response<Body>, NoiseError> {
        tracing::debug!("Received request from peer: {peer_id}");

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
            MessageType::GetNextCommand => self.handle_get_next_command(peer_id).await?,
            MessageType::GetChunk => self.handle_get_chunk(message.payload).await?,
            MessageType::KeysUpload => {
                self.handle_attestor_data(peer_id, message.payload, "keys upload")
                    .await?
            }
            MessageType::DnsSignature => {
                self.handle_attestor_data(peer_id, message.payload, "DNS signature")
                    .await?
            }
            MessageType::P2pSignatures => {
                self.handle_attestor_data(peer_id, message.payload, "P2P signatures")
                    .await?
            }
            MessageType::SubmissionSignatures => {
                self.handle_attestor_data(peer_id, message.payload, "submission signatures")
                    .await?
            }
            MessageType::KickSignatures => {
                self.handle_attestor_data(peer_id, message.payload, "kick signatures")
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

    /// Handle attestor requesting next command
    async fn handle_get_next_command(&self, peer_id: String) -> Result<Message, NoiseError> {
        // Mark attestor as connected on first command poll
        self.workflow_state
            .attestor_connected(peer_id.clone())
            .await;

        // Check if attestor has already completed current step
        let has_completed = self.workflow_state.has_attestor_completed(&peer_id).await;
        if has_completed {
            // Attestor has completed current step, tell them to wait
            tracing::debug!("Sending Wait to {peer_id} (already completed current step)");
            return Ok(Message::new_empty(MessageType::Wait));
        }

        // Get current command from workflow state
        if let Some(command) = self.workflow_state.current_command().await {
            tracing::info!("Sending command {command:?} to {peer_id}");
            // Include payload for commands that need data (e.g., SignDns, SignP2p)
            let payload = self.workflow_state.get_command_payload().await;
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
            // No command for attestors right now (coordinator-only step)
            tracing::debug!("Sending Wait to {peer_id} (coordinator-only step)");
            Ok(Message::new_empty(MessageType::Wait))
        }
    }

    /// Handle chunk request from attestor
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

    /// Handle attestor data upload (keys, signatures, etc.)
    async fn handle_attestor_data(
        &self,
        peer_id: String,
        payload: Vec<u8>,
        data_type: &str,
    ) -> Result<Message, NoiseError> {
        tracing::info!("Handling {data_type} from {peer_id}");

        self.workflow_state
            .store_attestor_data(peer_id.clone(), payload)
            .await;

        self.workflow_state.attestor_completed(peer_id).await;

        Ok(Message::new_empty(MessageType::Ack))
    }

    /// Handle status update from attestor
    async fn handle_status_update(
        &self,
        peer_id: String,
        payload: Vec<u8>,
    ) -> Result<Message, NoiseError> {
        let status = String::from_utf8_lossy(&payload);
        tracing::info!("Handling status update from {peer_id}: {status}");

        // Mark attestor as completed for this step
        self.workflow_state.attestor_completed(peer_id).await;

        Ok(Message::new_empty(MessageType::Ack))
    }
}
