use std::sync::Arc;

use bytes::Bytes;
use hyper::{Body, Request};
use secp256k1::PublicKey;
use tokio::net::TcpStream;

use crate::{
    config::{NetworkConfig, NodeConfig, Participant},
    noise::{
        Message, MessageType, NOISE_REQUEST_TIMEOUT, NoiseError, NoiseKeypair, parse_flexible_uri,
    },
};

/// Client for connecting to the coordinator
pub struct NoiseClient {
    node_config: Arc<NodeConfig>,
    #[allow(dead_code)]
    network_config: Arc<NetworkConfig>,
    keypair: Arc<NoiseKeypair>,
    coordinator: Participant,
    coordinator_pub_key: PublicKey,
}

impl NoiseClient {
    /// Create a new Noise client
    pub async fn new(
        node_config: NodeConfig,
        network_config: NetworkConfig,
    ) -> Result<Self, NoiseError> {
        // Load keypair
        let keypair = NoiseKeypair::from_file(&node_config.node.static_key_file).await?;

        // Get coordinator info
        let coordinator = network_config
            .get_coordinator()
            .map_err(|e| NoiseError::UnknownPeer(format!("Failed to get coordinator: {e}")))?
            .clone();

        // Parse coordinator's public key
        let pub_key_bytes =
            hex::decode(&coordinator.public_key).map_err(|_| NoiseError::InvalidMessage)?;
        let coordinator_pub_key =
            PublicKey::from_slice(&pub_key_bytes).map_err(|_| NoiseError::InvalidMessage)?;

        Ok(Self {
            node_config: Arc::new(node_config),
            network_config: Arc::new(network_config),
            keypair: Arc::new(keypair),
            coordinator,
            coordinator_pub_key,
        })
    }

    /// Send a message to the coordinator
    pub async fn send_message(&self, message: &Message) -> Result<Bytes, NoiseError> {
        let socket_addr = format!("{}:{}", self.coordinator.address, self.coordinator.port);

        tracing::debug!(
            "Sending message type {:?} to coordinator at {socket_addr}",
            message.msg_type
        );

        // Create HTTP request
        let uri = parse_flexible_uri(&format!("http://{socket_addr}/message"))?;
        let request_body = message.to_bytes();

        let request = Request::builder()
            .uri(uri)
            .method("POST")
            .body(Body::from(request_body))?;

        // Connect with timeout
        let tcp_stream =
            match tokio::time::timeout(NOISE_REQUEST_TIMEOUT, TcpStream::connect(&socket_addr))
                .await
            {
                Ok(Ok(stream)) => stream,
                Ok(Err(e)) => {
                    return Err(NoiseError::TcpConnectionFailed(format!(
                        "Failed to connect to {socket_addr}: {e}"
                    )));
                }
                Err(_) => return Err(NoiseError::TcpConnectionTimeout(socket_addr)),
            };

        // Derive PSK for this connection
        let psk = self.keypair.derive_psk(&self.coordinator_pub_key);

        // Use our participant_id as identity
        let identity = self.node_config.node.participant_id.as_bytes();

        // Create Noise initiator
        let initiator = tokio_noise::handshakes::nn_psk2::Initiator {
            psk: &psk,
            identity,
        };

        // Send request over Noise-encrypted channel
        let mut response = hyper_noise::client::send_request(
            tcp_stream,
            initiator,
            request,
            Some(NOISE_REQUEST_TIMEOUT),
        )
        .await?;

        // Check response status
        if response.status() != hyper::StatusCode::OK {
            return Err(NoiseError::BadStatusCode(response.status()));
        }

        // Read response body
        let resp_body_bytes = hyper::body::to_bytes(response.body_mut()).await?;

        tracing::debug!(
            "Received response from coordinator: {} bytes",
            resp_body_bytes.len()
        );

        Ok(resp_body_bytes)
    }

    /// Upload keys to coordinator
    pub async fn upload_keys(&self, keys_data: Vec<u8>) -> Result<(), NoiseError> {
        tracing::info!("Uploading keys to coordinator");

        let message = Message::new(MessageType::KeysUpload, keys_data);
        let response = self.send_message(&message).await?;

        // Parse response
        let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

        if resp_msg.msg_type != MessageType::Ack {
            return Err(NoiseError::InvalidMessage);
        }

        tracing::info!("Keys uploaded successfully");
        Ok(())
    }

    /// Send DNS signature to coordinator
    pub async fn send_dns_signature(&self, signature_data: Vec<u8>) -> Result<(), NoiseError> {
        tracing::info!("Sending DNS signature to coordinator");

        let message = Message::new(MessageType::DnsSignature, signature_data);
        let response = self.send_message(&message).await?;

        // Parse response
        let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

        if resp_msg.msg_type != MessageType::Ack {
            return Err(NoiseError::InvalidMessage);
        }

        tracing::info!("DNS signature sent successfully");
        Ok(())
    }

    /// Send P2P signatures to coordinator
    pub async fn send_p2p_signatures(&self, signatures_data: Vec<u8>) -> Result<(), NoiseError> {
        tracing::info!("Sending P2P signatures to coordinator");

        let message = Message::new(MessageType::P2pSignatures, signatures_data);
        let response = self.send_message(&message).await?;

        // Parse response
        let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

        if resp_msg.msg_type != MessageType::Ack {
            return Err(NoiseError::InvalidMessage);
        }

        tracing::info!("P2P signatures sent successfully");
        Ok(())
    }

    /// Send submission signatures to coordinator
    pub async fn send_submission_signatures(
        &self,
        signatures_data: Vec<u8>,
    ) -> Result<(), NoiseError> {
        tracing::info!("Sending submission signatures to coordinator");

        let message = Message::new(MessageType::SubmissionSignatures, signatures_data);
        let response = self.send_message(&message).await?;

        // Parse response
        let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

        if resp_msg.msg_type != MessageType::Ack {
            return Err(NoiseError::InvalidMessage);
        }

        tracing::info!("Submission signatures sent successfully");
        Ok(())
    }

    /// Send status update to coordinator
    pub async fn send_status(&self, status_data: Vec<u8>) -> Result<(), NoiseError> {
        tracing::info!("Sending status update to coordinator");

        let message = Message::new(MessageType::StatusUpdate, status_data);
        let response = self.send_message(&message).await?;

        // Parse response
        let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

        if resp_msg.msg_type != MessageType::Ack {
            return Err(NoiseError::InvalidMessage);
        }

        tracing::info!("Status update sent successfully");
        Ok(())
    }

    /// Poll coordinator for next command
    pub async fn get_next_command(&self) -> Result<MessageType, NoiseError> {
        tracing::debug!("Polling coordinator for next command");

        let message = Message::new_empty(MessageType::GetNextCommand);
        let response = self.send_message(&message).await?;

        // Parse response
        let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

        Ok(resp_msg.msg_type)
    }
}
