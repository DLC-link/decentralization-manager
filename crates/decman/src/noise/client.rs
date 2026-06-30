use std::{marker::PhantomData, sync::Arc, time::Duration};

use bytes::Bytes;
use hyper::{Body, Request};
use secp256k1::PublicKey;
use tokio::net::TcpStream;
use tokio_noise::handshakes::nn_psk2::Initiator;

use crate::{
    config::{NodeConfig, Peer},
    noise::{
        CHUNK_SIZE, MAX_CHUNK_COUNT, MAX_CHUNKED_TOTAL_SIZE, Message, MessageType,
        NOISE_CHUNK_TIMEOUT, NOISE_REQUEST_TIMEOUT, NoiseError, NoiseKeypair, is_transient,
        parse_flexible_uri, parse_public_key,
    },
};

/// Max attempts to fetch a single chunk (each on a fresh connection) before the
/// chunked transfer gives up. Lets a transient stall/reset recover in place
/// instead of restarting the whole download.
const CHUNK_FETCH_MAX_ATTEMPTS: usize = 3;

/// Client for connecting to the coordinator
pub struct NoiseClient {
    node_config: Arc<NodeConfig>,
    keypair: Arc<NoiseKeypair>,
    coordinator: Peer,
    coordinator_pub_key: PublicKey,
    /// The coordinator's run `instance_name`. Stamped onto every outbound
    /// command (`Message::instance`) that doesn't already carry one, so the
    /// coordinator's always-on listener routes this peer's traffic to the right
    /// concurrent run. Empty when the invite predated instance routing — the
    /// coordinator then falls back to its sole active run.
    route_instance: String,
    _p: PhantomData<()>,
}

impl NoiseClient {
    /// Create a new Noise client to connect to a specific coordinator.
    /// `route_instance` is the coordinator's run identifier used to route this
    /// peer's workflow commands; pass an empty string when unknown.
    pub async fn new(
        node_config: NodeConfig,
        coordinator: Peer,
        route_instance: String,
    ) -> Result<Self, NoiseError> {
        // Load keypair
        let keypair = NoiseKeypair::from_file(&node_config.key_file_path()).await?;

        // Parse coordinator's public key
        let coordinator_pub_key = parse_public_key(&coordinator.public_key)?;

        Ok(Self {
            node_config: Arc::new(node_config),
            keypair: Arc::new(keypair),
            coordinator,
            coordinator_pub_key,
            route_instance,
            _p: PhantomData,
        })
    }

    /// Send a message to the coordinator with the default control-plane timeout.
    pub async fn send_message(&self, message: &Message) -> Result<Bytes, NoiseError> {
        self.send_message_with_timeout(message, NOISE_REQUEST_TIMEOUT)
            .await
    }

    /// Send a message to the coordinator, applying `request_timeout` to both the
    /// TCP connect and the Noise/HTTP round-trip. Used so large chunk fetches can
    /// run on a longer budget than fast control-plane polls.
    pub async fn send_message_with_timeout(
        &self,
        message: &Message,
        request_timeout: Duration,
    ) -> Result<Bytes, NoiseError> {
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
        // Stamp the coordinator's run instance for routing unless the caller
        // already set one. encode_with_instance substitutes the routing key at
        // encode time — no clone of the (potentially chunk-sized) payload.
        let request_body = if message.instance.is_empty() && !self.route_instance.is_empty() {
            message.encode_with_instance(&self.route_instance)
        } else {
            message.to_bytes()
        };

        let request = Request::builder()
            .uri(uri)
            .method("POST")
            .body(Body::from(request_body))?;

        // Connect with timeout
        let tcp_stream =
            match tokio::time::timeout(request_timeout, TcpStream::connect(&socket_addr)).await {
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
            psk: &psk[..],
            identity,
        };

        // Send request over Noise-encrypted channel
        let mut response = hyper_noise::client::send_request(
            tcp_stream,
            initiator,
            request,
            Some(request_timeout),
        )
        .await?;

        // Check response status
        if response.status() != hyper::StatusCode::OK {
            return Err(NoiseError::BadStatusCode(response.status()));
        }

        // Read response body, bounded by `request_timeout`. `send_request`'s
        // timeout only covers receiving the response *head*, not streaming the
        // body — so without this, a stalled large body (e.g. a chunk that
        // freezes mid-transfer) would hang until the server closes the
        // connection instead of failing fast here and letting the caller retry.
        let resp_body_bytes =
            match tokio::time::timeout(request_timeout, hyper::body::to_bytes(response.body_mut()))
                .await
            {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(e)) => return Err(NoiseError::from(e)),
                Err(_) => return Err(NoiseError::RequestTimeout),
            };

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

    /// Tell the coordinator we are declining an outstanding invitation so it
    /// can fail its in-progress run with a descriptive error instead of
    /// hanging until timeout. Payload is the JSON-encoded
    /// `DeclineInvitationPayload`.
    pub async fn send_decline_invitation(&self, payload: Vec<u8>) -> Result<(), NoiseError> {
        self.send_and_verify_ack(
            MessageType::DeclineInvitation,
            payload,
            "Sending decline-invitation to coordinator",
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

        // Bound coordinator-supplied metadata before allocating or looping
        // (mirrors `send_noise_message_with_chunked_response`): a malicious or
        // buggy coordinator could otherwise advertise multi-GB sizes and OOM
        // us, or send a chunk_count inconsistent with total_size.
        if total_size > MAX_CHUNKED_TOTAL_SIZE || chunk_count > MAX_CHUNK_COUNT {
            tracing::warn!(
                "chunked-response metadata exceeds caps: total_size={total_size} \
                 chunk_count={chunk_count}"
            );
            return Err(NoiseError::InvalidMessage);
        }
        let expected_chunks = total_size.div_ceil(CHUNK_SIZE);
        if chunk_count != expected_chunks {
            tracing::warn!(
                "chunked-response chunk_count {chunk_count} inconsistent with total_size \
                 {total_size} (expected {expected_chunks})"
            );
            return Err(NoiseError::InvalidMessage);
        }

        tracing::info!(
            "Receiving chunked payload for {command:?}: {total_size} bytes in {chunk_count} chunks"
        );

        // Fetch all chunks. Each chunk is retried in place on a fresh
        // connection so a single slow/stalled chunk (e.g. transient coordinator
        // load) doesn't discard every already-downloaded chunk and force the
        // whole transfer to restart from chunk 0.
        let mut payload = Vec::with_capacity(total_size);
        for chunk_index in 0..chunk_count {
            let chunk_data = self.request_chunk_with_retry(chunk_index as u32).await?;
            payload.extend_from_slice(&chunk_data);
        }

        if payload.len() != total_size {
            tracing::warn!(
                "chunked-response assembly produced {} bytes but metadata declared {total_size}",
                payload.len()
            );
            return Err(NoiseError::InvalidMessage);
        }

        tracing::info!(
            "Received complete payload: {len} bytes",
            len = payload.len()
        );

        Ok(Message::new(command, payload))
    }

    /// Request a chunk, retrying transient failures (connection reset, timeout,
    /// truncated body) on a fresh connection up to `CHUNK_FETCH_MAX_ATTEMPTS`.
    /// Non-transient errors (bad message, decode failure) fail immediately.
    async fn request_chunk_with_retry(&self, chunk_index: u32) -> Result<Vec<u8>, NoiseError> {
        let mut attempt = 1;
        loop {
            match self.request_chunk(chunk_index).await {
                Ok(data) => return Ok(data),
                Err(e) if attempt < CHUNK_FETCH_MAX_ATTEMPTS && is_transient(&e) => {
                    tracing::warn!(
                        "chunk {chunk_index} fetch attempt {attempt}/{CHUNK_FETCH_MAX_ATTEMPTS} \
                         failed: {e}; retrying"
                    );
                    attempt += 1;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Request a specific chunk from the coordinator
    async fn request_chunk(&self, chunk_index: u32) -> Result<Vec<u8>, NoiseError> {
        let message = Message::new(MessageType::GetChunk, chunk_index.to_be_bytes().to_vec());
        // Chunks can be up to CHUNK_SIZE (1 MiB); give them the longer budget so
        // a slow stream isn't cut short by the control-plane timeout.
        let response = self
            .send_message_with_timeout(&message, NOISE_CHUNK_TIMEOUT)
            .await?;

        let resp_msg = Message::from_bytes(&response).map_err(|_| NoiseError::InvalidMessage)?;

        if resp_msg.msg_type != MessageType::Chunk || resp_msg.payload.len() < 4 {
            return Err(NoiseError::InvalidMessage);
        }

        // Chunk response: [chunk_index (4 bytes)] [chunk_data (variable)].
        // Verify the echoed index matches what we asked for so a server bug or
        // cross-peer cache mix-up can't silently corrupt the assembled payload.
        let received_index = u32::from_be_bytes([
            resp_msg.payload[0],
            resp_msg.payload[1],
            resp_msg.payload[2],
            resp_msg.payload[3],
        ]);
        if received_index != chunk_index {
            tracing::warn!(
                "chunk response carried wrong index: requested {chunk_index}, got {received_index}"
            );
            return Err(NoiseError::InvalidMessage);
        }

        Ok(resp_msg.payload[4..].to_vec())
    }
}
