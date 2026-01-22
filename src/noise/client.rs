use std::{marker::PhantomData, sync::Arc};

use bytes::Bytes;
use hyper::{Body, Request};
use secp256k1::PublicKey;
use tokio::net::TcpStream;
use tokio_noise::handshakes::nn_psk2::Initiator;

use crate::{
    config::{NodeConfig, Peer},
    noise::{
        Message, MessageType, NOISE_REQUEST_TIMEOUT, NoiseError, NoiseKeypair, parse_flexible_uri,
        parse_public_key,
    },
};

/// Client for connecting to the coordinator
pub struct NoiseClient {
    node_config: Arc<NodeConfig>,
    keypair: Arc<NoiseKeypair>,
    coordinator: Peer,
    coordinator_pub_key: PublicKey,
    _p: PhantomData<()>,
}

impl NoiseClient {
    /// Create a new Noise client to connect to a specific coordinator
    pub async fn new(node_config: NodeConfig, coordinator: Peer) -> Result<Self, NoiseError> {
        // Load keypair
        let keypair = NoiseKeypair::from_file(&node_config.key_file_path()).await?;

        // Parse coordinator's public key
        let coordinator_pub_key = parse_public_key(&coordinator.public_key)?;

        Ok(Self {
            node_config: Arc::new(node_config),
            keypair: Arc::new(keypair),
            coordinator,
            coordinator_pub_key,
            _p: PhantomData,
        })
    }

    /// Send a message to the coordinator
    pub async fn send_message(&self, message: &Message) -> Result<Bytes, NoiseError> {
        let socket_addr = format!(
            "{address}:{port}",
            address = self.coordinator.address,
            port = self.coordinator.port
        );

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
        let identity = self.node_config.participant_id().to_string();
        let identity = identity.as_bytes();

        // Create Noise initiator
        let initiator = Initiator {
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
            "Received response from coordinator: {count} bytes",
            count = resp_body_bytes.len()
        );

        Ok(resp_body_bytes)
    }

    /// Helper method to send a message and verify Ack response
    async fn send_and_verify_ack(
        &self,
        msg_type: MessageType,
        payload: Vec<u8>,
        action: &str,
    ) -> Result<(), NoiseError> {
        tracing::info!("{action}");

        let message = Message::new(msg_type, payload);
        let response = self.send_message(&message).await?;

        // Parse response
        let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

        if resp_msg.msg_type != MessageType::Ack {
            return Err(NoiseError::InvalidMessage);
        }

        tracing::info!("{action} completed successfully");
        Ok(())
    }

    /// Upload keys to coordinator
    pub async fn upload_keys(&self, keys_data: Vec<u8>) -> Result<(), NoiseError> {
        self.send_and_verify_ack(
            MessageType::KeysUpload,
            keys_data,
            "Uploading keys to coordinator",
        )
        .await
    }

    /// Send DNS signature to coordinator
    pub async fn send_dns_signature(&self, signature_data: Vec<u8>) -> Result<(), NoiseError> {
        self.send_and_verify_ack(
            MessageType::DnsSignature,
            signature_data,
            "Sending DNS signature to coordinator",
        )
        .await
    }

    /// Send P2P signatures to coordinator
    pub async fn send_p2p_signatures(&self, signatures_data: Vec<u8>) -> Result<(), NoiseError> {
        self.send_and_verify_ack(
            MessageType::P2pSignatures,
            signatures_data,
            "Sending P2P signatures to coordinator",
        )
        .await
    }

    /// Send submission signatures to coordinator
    pub async fn send_submission_signatures(
        &self,
        signatures_data: Vec<u8>,
    ) -> Result<(), NoiseError> {
        self.send_and_verify_ack(
            MessageType::SubmissionSignatures,
            signatures_data,
            "Sending submission signatures to coordinator",
        )
        .await
    }

    pub async fn send_kick_signatures(&self, signatures_data: Vec<u8>) -> Result<(), NoiseError> {
        self.send_and_verify_ack(
            MessageType::KickSignatures,
            signatures_data,
            "Sending kick signatures to coordinator",
        )
        .await
    }

    /// Send status update to coordinator
    pub async fn send_status(&self, status_data: Vec<u8>) -> Result<(), NoiseError> {
        self.send_and_verify_ack(
            MessageType::StatusUpdate,
            status_data,
            "Sending status update to coordinator",
        )
        .await
    }

    /// Poll coordinator for next command, returning just the command type
    pub async fn get_next_command(&self) -> Result<MessageType, NoiseError> {
        let msg = self.get_next_command_with_payload().await?;
        Ok(msg.msg_type)
    }

    /// Poll coordinator for next command, returning full message with payload
    pub async fn get_next_command_with_payload(&self) -> Result<Message, NoiseError> {
        tracing::debug!("Polling coordinator for next command");

        let message = Message::new_empty(MessageType::GetNextCommand);
        let response = self.send_message(&message).await?;

        // Parse response
        let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

        // Handle chunked transfer
        if resp_msg.msg_type == MessageType::ChunkedCommand {
            return self.receive_chunked_payload(&resp_msg.payload).await;
        }

        Ok(resp_msg)
    }

    /// Receive a chunked payload from the coordinator
    async fn receive_chunked_payload(&self, meta: &[u8]) -> Result<Message, NoiseError> {
        if meta.len() < 10 {
            return Err(NoiseError::InvalidMessage);
        }

        // Parse metadata: [command_type (2 bytes)] [total_size (4 bytes)] [chunk_count (4 bytes)]
        let command_type = u16::from_be_bytes([meta[0], meta[1]]);
        let total_size = u32::from_be_bytes([meta[2], meta[3], meta[4], meta[5]]) as usize;
        let chunk_count = u32::from_be_bytes([meta[6], meta[7], meta[8], meta[9]]) as usize;

        let command =
            MessageType::try_from(command_type).map_err(|_| NoiseError::InvalidMessage)?;

        tracing::info!(
            "Receiving chunked payload for {command:?}: {total_size} bytes in {chunk_count} chunks"
        );

        // Fetch all chunks
        let mut payload = Vec::with_capacity(total_size);
        for chunk_index in 0..chunk_count {
            let chunk_data = self.request_chunk(chunk_index as u32).await?;
            payload.extend_from_slice(&chunk_data);
        }

        tracing::info!(
            "Received complete payload: {len} bytes",
            len = payload.len()
        );

        Ok(Message::new(command, payload))
    }

    /// Request a specific chunk from the coordinator
    async fn request_chunk(&self, chunk_index: u32) -> Result<Vec<u8>, NoiseError> {
        let message = Message::new(MessageType::GetChunk, chunk_index.to_be_bytes().to_vec());
        let response = self.send_message(&message).await?;

        let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

        if resp_msg.msg_type != MessageType::Chunk || resp_msg.payload.len() < 4 {
            return Err(NoiseError::InvalidMessage);
        }

        // Chunk response: [chunk_index (4 bytes)] [chunk_data (variable)]
        Ok(resp_msg.payload[4..].to_vec())
    }
}
