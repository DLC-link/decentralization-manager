use std::{collections::HashMap, sync::Arc};

use hyper::{Body, Request, Response, StatusCode};
use secp256k1::PublicKey;
use tokio::net::TcpListener;

use crate::{
    config::{NetworkConfig, NodeConfig},
    noise::{parse_public_key, Message, MessageType, NoiseError, NoiseKeypair, NOISE_REQUEST_TIMEOUT},
    workflow::{state::WorkflowStep, WorkflowState},
};

/// Coordinator server that accepts connections from attestors
pub struct NoiseServer<S: WorkflowStep + 'static> {
    node_config: Arc<NodeConfig>,
    network_config: Arc<NetworkConfig>,
    keypair: Arc<NoiseKeypair>,
    peer_keys: HashMap<String, PublicKey>,
    workflow_state: Arc<WorkflowState<S>>,
}

impl<S: WorkflowStep + 'static> NoiseServer<S> {
    pub async fn new(
        node_config: NodeConfig,
        network_config: NetworkConfig,
        initial_step: S,
    ) -> Result<Self, NoiseError> {
        let keypair = NoiseKeypair::from_file(&node_config.node.static_key_file).await?;

        let mut peer_keys = HashMap::new();
        for participant in &network_config.participants {
            if participant.id == node_config.node.node_id {
                continue;
            }

            let pub_key = parse_public_key(&participant.public_key)?;
            peer_keys.insert(participant.id.clone(), pub_key);
        }

        let expected_attestors: Vec<String> = network_config
            .participants
            .iter()
            .filter(|p| p.id != node_config.node.node_id)
            .map(|p| p.id.clone())
            .collect();
        let workflow_state = WorkflowState::new(initial_step, expected_attestors);

        Ok(Self {
            node_config: Arc::new(node_config),
            network_config: Arc::new(network_config),
            keypair: Arc::new(keypair),
            peer_keys,
            workflow_state,
        })
    }

    pub fn get_workflow_state(&self) -> Arc<WorkflowState<S>> {
        self.workflow_state.clone()
    }

    /// Start the server and listen for connections
    pub async fn start(self: Arc<Self>) -> Result<(), NoiseError> {
        let listen_addr = format!(
            "{}:{}",
            self.node_config.node.listen_address,
            self.network_config
                .get_participant(&self.node_config.node.node_id)
                .ok_or_else(|| { NoiseError::UnknownPeer(self.node_config.node.node_id.clone()) })?
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
                tokio_noise::handshakes::nn_psk2::Responder::new(
                    move |identity: &[u8]| -> Option<[u8; 32]> {
                        // Identity is the participant_id
                        let peer_id = std::str::from_utf8(identity).ok()?;
                        let peer_pub_key = peer_keys.get(peer_id)?;

                        // Derive PSK using ECDH
                        let psk = secp256k1::ecdh::SharedSecret::new(peer_pub_key, &secret_key);
                        Some(psk.secret_bytes())
                    },
                )
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
            MessageType::KeysUpload => self.handle_keys_upload(peer_id, message.payload).await?,
            MessageType::DnsSignature => {
                self.handle_dns_signature(peer_id, message.payload).await?
            }
            MessageType::P2pSignatures => {
                self.handle_p2p_signatures(peer_id, message.payload).await?
            }
            MessageType::SubmissionSignatures => {
                self.handle_submission_signatures(peer_id, message.payload)
                    .await?
            }
            MessageType::KickSignatures => {
                self.handle_kick_signatures(peer_id, message.payload).await?
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
            Ok(Message::new_empty(command))
        } else {
            // No command for attestors right now (coordinator-only step)
            tracing::debug!("Sending Wait to {peer_id} (coordinator-only step)");
            Ok(Message::new_empty(MessageType::Wait))
        }
    }

    /// Handle keys upload from attestor
    async fn handle_keys_upload(
        &self,
        peer_id: String,
        payload: Vec<u8>,
    ) -> Result<Message, NoiseError> {
        tracing::info!("Handling keys upload from {peer_id}");

        // Store the keys data
        self.workflow_state
            .store_attestor_data(peer_id.clone(), payload)
            .await;

        // Mark attestor as completed for this step
        self.workflow_state.attestor_completed(peer_id).await;

        Ok(Message::new_empty(MessageType::Ack))
    }

    /// Handle DNS signature from attestor
    async fn handle_dns_signature(
        &self,
        peer_id: String,
        payload: Vec<u8>,
    ) -> Result<Message, NoiseError> {
        tracing::info!("Handling DNS signature from {peer_id}");

        // Store the signature data
        self.workflow_state
            .store_attestor_data(peer_id.clone(), payload)
            .await;

        // Mark attestor as completed for this step
        self.workflow_state.attestor_completed(peer_id).await;

        Ok(Message::new_empty(MessageType::Ack))
    }

    /// Handle P2P signatures from attestor
    async fn handle_p2p_signatures(
        &self,
        peer_id: String,
        payload: Vec<u8>,
    ) -> Result<Message, NoiseError> {
        tracing::info!("Handling P2P signatures from {peer_id}");

        // Store the signatures data
        self.workflow_state
            .store_attestor_data(peer_id.clone(), payload)
            .await;

        // Mark attestor as completed for this step
        self.workflow_state.attestor_completed(peer_id).await;

        Ok(Message::new_empty(MessageType::Ack))
    }

    /// Handle submission signatures from attestor
    async fn handle_submission_signatures(
        &self,
        peer_id: String,
        payload: Vec<u8>,
    ) -> Result<Message, NoiseError> {
        tracing::info!("Handling submission signatures from {peer_id}");

        // Store the signatures data
        self.workflow_state
            .store_attestor_data(peer_id.clone(), payload)
            .await;

        // Mark attestor as completed for this step
        self.workflow_state.attestor_completed(peer_id).await;

        Ok(Message::new_empty(MessageType::Ack))
    }

    /// Handle kick signatures from attestor
    async fn handle_kick_signatures(
        &self,
        peer_id: String,
        payload: Vec<u8>,
    ) -> Result<Message, NoiseError> {
        tracing::info!("Handling kick signatures from {peer_id}");

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
