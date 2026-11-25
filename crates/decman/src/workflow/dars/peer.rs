//! The peer side of a Dars run: read its DARs from the coordinator.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, LazyLock, Mutex, PoisonError},
};

use anyhow::Context;

use crate::{
    config::{NodeConfig, NoiseRetryConfig, Peer},
    consts::MAX_DAR_PULL_TOTAL,
    error::Result,
    noise::{
        MAX_CHUNKED_TOTAL_SIZE, Message, MessageType, NOISE_CHUNK_TIMEOUT, NoiseKeypair,
        client::CHUNK_FETCH_MAX_ATTEMPTS, parse_chunked_command_metadata, parse_public_key,
        receive_chunked_response, send_noise_message_with_retry,
    },
    server::RequestDarPayload,
    workflow::validation::hash_dar,
};

/// The budget for each request of a pull. It matches the chunk budget of the
/// bundle download that the pull replaces.
const PULL_RETRY: NoiseRetryConfig = NoiseRetryConfig {
    per_attempt_timeout_secs: NOISE_CHUNK_TIMEOUT.as_secs(),
    max_attempts: CHUNK_FETCH_MAX_ATTEMPTS,
    backoff_ms: 1_000,
};

/// The most bytes of a coordinator's refusal that reach the log and the run
/// record.
const MAX_REFUSAL_LEN: usize = 512;

/// One lock for each coordinator, by its public key. The coordinator keeps
/// one chunked `RequestDar` answer per requesting node, so a second fetch
/// from this node would replace the chunks the first is still reading.
static COORDINATOR_TURNS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Mutex::default);

fn coordinator_turn(public_key: &str) -> Arc<tokio::sync::Mutex<()>> {
    COORDINATOR_TURNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(public_key.to_string())
        .or_default()
        .clone()
}

/// A pull that the coordinator's sizes rule out. The sizes are the same on
/// every attempt, so the peer ends the run instead of retrying.
#[derive(Debug, thiserror::Error)]
pub enum OversizedPull {
    #[error("{filename} is {size} bytes, above the limit of {limit} bytes for one DAR")]
    Dar {
        filename: String,
        size: usize,
        limit: usize,
    },
    #[error(
        "the DARs of this run add up to {total} bytes so far, above the cap of {cap} bytes \
         for one run"
    )]
    Run { total: usize, cap: usize },
}

/// What a peer needs to read the DARs of its accepted invitation one by one.
#[derive(Debug)]
pub struct DarPull {
    coordinator: Peer,
    workflow_instance: String,
    filenames: Vec<String>,
    hashes: Vec<String>,
}

impl DarPull {
    /// Plan the pull for an accepted invitation.
    ///
    /// Returns `None` when the invitation lacks what a pull needs: the
    /// coordinator's run instance, at least one DAR, and one hash for each
    /// DAR. The peer then takes the coordinator's bundle.
    ///
    /// # Errors
    ///
    /// Errors when the invitation names one DAR twice. The pull would read it
    /// twice, and the set check would refuse it only after every byte arrived.
    pub fn new(
        coordinator: &Peer,
        workflow_instance: &str,
        dar_filenames: &[String],
        dar_hashes: &[String],
    ) -> Result<Option<Self>> {
        let pinned = !dar_filenames.is_empty() && dar_hashes.len() == dar_filenames.len();
        if !pinned || workflow_instance.is_empty() {
            return Ok(None);
        }
        let distinct: BTreeSet<&str> = dar_filenames.iter().map(String::as_str).collect();
        if distinct.len() != dar_filenames.len() {
            anyhow::bail!("the accepted invitation names the same DAR twice");
        }
        Ok(Some(Self {
            coordinator: coordinator.clone(),
            workflow_instance: workflow_instance.to_string(),
            filenames: dar_filenames.to_vec(),
            hashes: dar_hashes.to_vec(),
        }))
    }

    /// Read every DAR of the invitation from the coordinator, in index order.
    ///
    /// # Errors
    ///
    /// Errors on the first DAR the coordinator does not serve, or that does
    /// not hash to the value pinned at its index, and names it. Errors with
    /// [`OversizedPull`] once the announced sizes pass a limit.
    pub async fn fetch_all(&self, config: &NodeConfig) -> Result<Vec<(String, Vec<u8>)>> {
        self.fetch_within(config, MAX_DAR_PULL_TOTAL).await
    }

    /// As [`Self::fetch_all`], with `cap` bytes for the run's total.
    pub(super) async fn fetch_within(
        &self,
        config: &NodeConfig,
        cap: usize,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let count = self.filenames.len();
        let mut total = 0usize;
        let mut dars = Vec::with_capacity(count);
        for (index, (filename, pinned)) in self.filenames.iter().zip(&self.hashes).enumerate() {
            let request = RequestDarPayload {
                workflow_instance: self.workflow_instance.clone(),
                index,
                filename: filename.clone(),
            };
            let admit = |size: usize| {
                total = total.saturating_add(size);
                if total > cap {
                    return Err(OversizedPull::Run { total, cap }.into());
                }
                Ok(())
            };
            let dar =
                fetch_dar_from_coordinator(config, &self.coordinator, &request, &PULL_RETRY, admit)
                    .await
                    .with_context(|| format!("DAR {index} ({filename})"))?;
            // Stop at the first DAR that misses its pin, before reading the
            // rest. The whole set is checked again before any upload.
            let actual = hash_dar(&dar);
            if actual != *pinned {
                anyhow::bail!(
                    "DAR {filename} hashes to {actual} but the invitation pinned {pinned}"
                );
            }
            tracing::info!(
                "Read DAR {position}/{count} from the coordinator: {filename}, {len} bytes",
                position = index + 1,
                len = dar.len()
            );
            dars.push((filename.clone(), dar));
        }
        Ok(dars)
    }
}

/// Ask the coordinator for one DAR over Noise, reassembling a chunked answer.
///
/// `admit` sees the DAR's size first: the announced size of a chunked answer,
/// before any chunk is read, or the length of an inline one. An error from it
/// ends the fetch. Only one fetch at a time runs against a coordinator.
///
/// # Errors
///
/// Errors when the coordinator refuses, when `admit` refuses the size, when
/// the DAR is larger than `MAX_CHUNKED_TOTAL_SIZE` ([`OversizedPull::Dar`]),
/// or when the transfer fails.
pub async fn fetch_dar_from_coordinator(
    config: &NodeConfig,
    coordinator: &Peer,
    request: &RequestDarPayload,
    retry: &NoiseRetryConfig,
    admit: impl FnOnce(usize) -> Result,
) -> Result<Vec<u8>> {
    let turn = coordinator_turn(&coordinator.public_key);
    let _turn = turn.lock().await;

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

    match response.msg_type {
        MessageType::ChunkedCommand => {
            let (_, size, _) = parse_chunked_command_metadata(&response.payload)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if size > MAX_CHUNKED_TOTAL_SIZE {
                return Err(OversizedPull::Dar {
                    filename: request.filename.clone(),
                    size,
                    limit: MAX_CHUNKED_TOTAL_SIZE,
                }
                .into());
            }
            admit(size)?;
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
        MessageType::Data => admit(response.payload.len())?,
        _ => {}
    }

    match response.msg_type {
        MessageType::Data => Ok(response.payload),
        MessageType::Error => Err(anyhow::anyhow!("{}", refusal_text(&response.payload))),
        other => Err(anyhow::anyhow!("unexpected answer {other:?}")),
    }
}

/// A coordinator's refusal as text for the log and the run record. Control
/// characters are escaped, so a refusal cannot forge log lines, and the text
/// is cut at [`MAX_REFUSAL_LEN`] bytes.
fn refusal_text(payload: &[u8]) -> String {
    let mut escaped = String::new();
    for c in String::from_utf8_lossy(payload).chars() {
        if c.is_control() {
            escaped.extend(c.escape_default());
        } else {
            escaped.push(c);
        }
    }
    if escaped.len() <= MAX_REFUSAL_LEN {
        return escaped;
    }
    let mut end = MAX_REFUSAL_LEN;
    while !escaped.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &escaped[..end])
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

        let pull = DarPull::new(&coordinator()?, "run-1", &names(2), &hashes)?
            .ok_or_else(|| anyhow::anyhow!("no pull planned"))?;

        assert_eq!(pull.workflow_instance, "run-1");
        assert_eq!(pull.filenames, names(2));
        assert_eq!(pull.hashes, hashes);
        Ok(())
    }

    #[test]
    fn an_invitation_without_the_run_instance_takes_the_bundle() -> anyhow::Result<()> {
        let hashes = vec!["aa".to_string()];

        assert!(DarPull::new(&coordinator()?, "", &names(1), &hashes)?.is_none());
        Ok(())
    }

    #[test]
    fn an_invitation_without_a_hash_for_each_dar_takes_the_bundle() -> anyhow::Result<()> {
        let partial = vec!["aa".to_string()];

        assert!(DarPull::new(&coordinator()?, "run-1", &names(2), &[])?.is_none());
        assert!(DarPull::new(&coordinator()?, "run-1", &names(2), &partial)?.is_none());
        Ok(())
    }

    #[test]
    fn an_invitation_without_dars_takes_the_bundle() -> anyhow::Result<()> {
        assert!(DarPull::new(&coordinator()?, "run-1", &[], &[])?.is_none());
        Ok(())
    }

    #[test]
    fn an_invitation_that_names_a_dar_twice_is_refused() -> anyhow::Result<()> {
        let twice = vec!["a.dar".to_string(), "a.dar".to_string()];
        let hashes = vec!["aa".to_string(), "bb".to_string()];

        let error = DarPull::new(&coordinator()?, "run-1", &twice, &hashes)
            .expect_err("a repeated name was accepted");

        assert!(
            error.to_string().contains("names the same DAR twice"),
            "{error}"
        );
        Ok(())
    }

    /// The text goes to the log and the run record, so it must not carry a
    /// line break or a terminal escape, and must stay short.
    #[test]
    fn a_refusal_is_escaped_and_cut() {
        let refusal = format!("no such DAR\n\u{1b}[31mforged line{}", "x".repeat(2_000));

        let text = refusal_text(refusal.as_bytes());

        assert!(
            text.starts_with("no such DAR\\n\\u{1b}[31mforged line"),
            "{text}"
        );
        assert!(!text.chars().any(char::is_control), "{text}");
        assert_eq!(text.len(), MAX_REFUSAL_LEN + "…".len());
        assert!(text.ends_with('…'));
    }

    #[test]
    fn a_short_refusal_is_kept_whole() {
        assert_eq!(
            refusal_text(b"run has no DAR at index 2"),
            "run has no DAR at index 2"
        );
    }
}
