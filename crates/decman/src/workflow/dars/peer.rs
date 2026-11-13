//! The peer side of a Dars run: read its DARs from the coordinator.

use anyhow::Context;

use crate::{
    config::{NodeConfig, NoiseRetryConfig, Peer},
    error::Result,
    noise::{
        MAX_CHUNKED_TOTAL_SIZE, Message, MessageType, NOISE_CHUNK_TIMEOUT, NoiseKeypair,
        client::CHUNK_FETCH_MAX_ATTEMPTS, parse_chunked_command_metadata, parse_public_key,
        receive_chunked_response, send_noise_message_with_retry,
    },
    server::RequestDarPayload,
};

/// The budget for each request of a pull. It matches the chunk budget of the
/// bundle download that the pull replaces.
const PULL_RETRY: NoiseRetryConfig = NoiseRetryConfig {
    per_attempt_timeout_secs: NOISE_CHUNK_TIMEOUT.as_secs(),
    max_attempts: CHUNK_FETCH_MAX_ATTEMPTS,
    backoff_ms: 1_000,
};

/// What a peer needs to read the DARs of its accepted invitation one by one.
#[derive(Debug)]
pub struct DarPull {
    coordinator: Peer,
    workflow_instance: String,
    filenames: Vec<String>,
}

impl DarPull {
    /// Plan the pull for an accepted invitation.
    ///
    /// Returns `None` when the invitation lacks what a pull needs: the
    /// coordinator's run instance, at least one DAR, and one hash for each
    /// DAR. The peer then takes the coordinator's bundle.
    pub fn new(
        coordinator: &Peer,
        workflow_instance: &str,
        dar_filenames: &[String],
        dar_hashes: &[String],
    ) -> Option<Self> {
        let pinned = !dar_filenames.is_empty() && dar_hashes.len() == dar_filenames.len();
        (pinned && !workflow_instance.is_empty()).then(|| Self {
            coordinator: coordinator.clone(),
            workflow_instance: workflow_instance.to_string(),
            filenames: dar_filenames.to_vec(),
        })
    }

    /// Read every DAR of the invitation from the coordinator, in index order.
    ///
    /// # Errors
    ///
    /// Errors on the first DAR the coordinator does not serve, and names it.
    pub async fn fetch_all(&self, config: &NodeConfig) -> Result<Vec<(String, Vec<u8>)>> {
        let total = self.filenames.len();
        let mut dars = Vec::with_capacity(total);
        for (index, filename) in self.filenames.iter().enumerate() {
            let request = RequestDarPayload {
                workflow_instance: self.workflow_instance.clone(),
                index,
                filename: filename.clone(),
            };
            let dar = fetch_dar_from_coordinator(config, &self.coordinator, &request, &PULL_RETRY)
                .await
                .with_context(|| format!("DAR {index} ({filename})"))?;
            tracing::info!(
                "Read DAR {count}/{total} from the coordinator: {filename}, {len} bytes",
                count = index + 1,
                len = dar.len()
            );
            dars.push((filename.clone(), dar));
        }
        Ok(dars)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canton_id::CantonId;

    fn coordinator() -> anyhow::Result<Peer> {
        Ok(Peer {
            participant_id: CantonId::parse(&format!("coordinator::1220{}", "b".repeat(64)))?,
            name: "Coordinator".into(),
            address: "127.0.0.1".into(),
            port: 1,
            public_key: String::new(),
            party: None,
        })
    }

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("pkg-{i}.dar")).collect()
    }

    #[test]
    fn an_invitation_that_pins_every_dar_is_pulled() -> anyhow::Result<()> {
        let hashes = vec!["aa".to_string(), "bb".to_string()];

        let pull = DarPull::new(&coordinator()?, "run-1", &names(2), &hashes)
            .ok_or_else(|| anyhow::anyhow!("no pull planned"))?;

        assert_eq!(pull.workflow_instance, "run-1");
        assert_eq!(pull.filenames, names(2));
        Ok(())
    }

    #[test]
    fn an_invitation_without_the_run_instance_takes_the_bundle() -> anyhow::Result<()> {
        let hashes = vec!["aa".to_string()];

        assert!(DarPull::new(&coordinator()?, "", &names(1), &hashes).is_none());
        Ok(())
    }

    #[test]
    fn an_invitation_without_a_hash_for_each_dar_takes_the_bundle() -> anyhow::Result<()> {
        let partial = vec!["aa".to_string()];

        assert!(DarPull::new(&coordinator()?, "run-1", &names(2), &[]).is_none());
        assert!(DarPull::new(&coordinator()?, "run-1", &names(2), &partial).is_none());
        Ok(())
    }

    #[test]
    fn an_invitation_without_dars_takes_the_bundle() -> anyhow::Result<()> {
        assert!(DarPull::new(&coordinator()?, "run-1", &[], &[]).is_none());
        Ok(())
    }
}
