//! The peer side of a Dars run: read its DARs from the coordinator.

use crate::{
    config::{NodeConfig, NoiseRetryConfig, Peer},
    error::Result,
    noise::{
        MAX_CHUNKED_TOTAL_SIZE, Message, MessageType, NoiseKeypair, parse_chunked_command_metadata,
        parse_public_key, receive_chunked_response, send_noise_message_with_retry,
    },
    server::RequestDarPayload,
};

/// Ask the coordinator for one DAR over Noise, reassembling a chunked answer.
///
/// # Errors
///
/// Errors when the coordinator refuses, when the DAR is larger than
/// `MAX_CHUNKED_TOTAL_SIZE`, or when the transfer fails.
pub async fn fetch_dar_from_coordinator(
    config: &NodeConfig,
    coordinator: &Peer,
    request: &RequestDarPayload,
    retry: &NoiseRetryConfig,
) -> Result<Vec<u8>> {
    let peer_pub_key = parse_public_key(&coordinator.public_key)
        .map_err(|e| anyhow::anyhow!("the coordinator's public key does not parse: {e}"))?;
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let psk = keypair.derive_psk(&peer_pub_key);
    let identity = config.participant_id().to_string();

    let message = Message::new(MessageType::RequestDar, serde_json::to_vec(request)?);
    let response = send_noise_message_with_retry(
        &coordinator.address,
        coordinator.port,
        &psk,
        identity.as_bytes(),
        &message,
        retry,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut response = Message::from_bytes(&response)
        .map_err(|_| anyhow::anyhow!("the coordinator's answer does not parse"))?;

    if response.msg_type == MessageType::ChunkedCommand {
        let (_, size, _) = parse_chunked_command_metadata(&response.payload)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if size > MAX_CHUNKED_TOTAL_SIZE {
            anyhow::bail!(
                "{filename} is {size} bytes, above the limit of {MAX_CHUNKED_TOTAL_SIZE} bytes \
                 for one DAR",
                filename = request.filename
            );
        }
        response = receive_chunked_response(
            &coordinator.address,
            coordinator.port,
            &psk,
            identity.as_bytes(),
            &response.payload,
            retry,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    match response.msg_type {
        MessageType::Data => Ok(response.payload),
        MessageType::Error => Err(anyhow::anyhow!(
            "{}",
            String::from_utf8_lossy(&response.payload)
        )),
        other => Err(anyhow::anyhow!("unexpected answer {other:?}")),
    }
}
