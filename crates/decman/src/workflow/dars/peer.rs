//! The peer side of a Dars run: read its DARs from the coordinator.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, LazyLock, Mutex, PoisonError},
    time::Duration,
};

use anyhow::Context;

use crate::{
    config::{NodeConfig, NoiseRetryConfig, Peer},
    consts::MAX_DAR_PULL_TOTAL,
    error::Result,
    noise::{
        CHUNK_SIZE, MAX_CHUNKED_TOTAL_SIZE, MAX_REPLY_SIZE, Message, MessageType,
        NOISE_CHUNK_TIMEOUT, NoiseKeypair, client::CHUNK_FETCH_MAX_ATTEMPTS,
        parse_chunked_command_metadata, parse_public_key, receive_chunked_response,
        send_noise_message_within,
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

/// The most bytes of one field a coordinator controls, such as a DAR's
/// filename or its pinned hash, that reach the log and the run record.
pub(crate) const MAX_FIELD_LEN: usize = 128;

/// One lock for each coordinator, by its public key. The coordinator keeps
/// one chunked `RequestDar` answer per requesting node, so a second fetch
/// from this node would replace the chunks the first is still reading.
static COORDINATOR_TURNS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Mutex::default);

/// How long a fetch waits for its turn at a coordinator: as long as one pull
/// of a 16 MiB DAR takes when each of its requests, the `RequestDar` and 16
/// chunks, uses the full per-request timeout of a pull. That is 425 seconds.
/// A waiter past it fails with a clear error instead of waiting forever.
///
/// The wait covers one attempt per request. A fetch that retries its
/// requests can hold the turn longer. A pull that waits past this then fails
/// its step, and the step retries.
const TURN_WAIT: Duration = Duration::from_secs(
    (1 + MAX_CHUNKED_TOTAL_SIZE.div_ceil(CHUNK_SIZE)) as u64 * NOISE_CHUNK_TIMEOUT.as_secs(),
);

fn coordinator_turn(public_key: &str) -> Arc<tokio::sync::Mutex<()>> {
    COORDINATOR_TURNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(public_key.to_string())
        .or_default()
        .clone()
}

/// Wait at most `limit` for `turn`.
async fn wait_for_turn(
    turn: &tokio::sync::Mutex<()>,
    limit: Duration,
) -> Result<tokio::sync::MutexGuard<'_, ()>> {
    tokio::time::timeout(limit, turn.lock()).await.map_err(|_| {
        anyhow::anyhow!(
            "another DAR fetch from this coordinator has run for {} seconds; try again once \
             it ends",
            limit.as_secs()
        )
    })
}

/// A pull that fails the same way on every attempt: the coordinator serves
/// the same stored DARs, and the invitation does not change. The peer ends
/// the run instead of retrying.
#[derive(Debug, thiserror::Error)]
pub enum PullRefused {
    #[error("{filename} is {size} bytes, above the limit of {limit} bytes for one DAR")]
    DarTooLarge {
        filename: String,
        size: usize,
        limit: usize,
    },
    #[error(
        "the DARs of this run add up to {} MiB ({total} bytes) so far, above the cap of {} \
         MiB for one run. Send the DARs in several runs of at most {} MiB each",
        mib(*total, Round::Up),
        mib(*cap, Round::Down),
        mib(*cap, Round::Down)
    )]
    RunTooLarge { total: usize, cap: usize },
    #[error(
        "DAR {filename} hashes to {actual} but the invitation pinned {pinned}. Ask the \
         coordinator's operator to check this DAR, then start a new run"
    )]
    HashMiss {
        filename: String,
        actual: String,
        pinned: String,
    },
    #[error(
        "the coordinator sent no DARs, and this node cannot pull them: a pull needs the \
         coordinator's run instance and one hash for each DAR in the invitation. The \
         coordinator must send the bundle or be upgraded"
    )]
    NotPullable,
}

/// Which way [`mib`] rounds a size that does not fill its last tenth.
#[derive(Clone, Copy, Debug)]
enum Round {
    Up,
    Down,
}

/// `bytes` in MiB: whole when it divides evenly, else to one decimal. A
/// total rounds up and a cap rounds down, so a total above the cap never
/// reads as equal to it.
fn mib(bytes: usize, round: Round) -> String {
    const MIB: usize = 1024 * 1024;
    if bytes.is_multiple_of(MIB) {
        return (bytes / MIB).to_string();
    }
    let tenths = match round {
        Round::Up => (bytes * 10).div_ceil(MIB),
        Round::Down => bytes * 10 / MIB,
    };
    format!("{}.{}", tenths / 10, tenths % 10)
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
    /// [`PullRefused`] once the announced sizes pass a limit or a DAR misses
    /// its pin.
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
                    return Err(PullRefused::RunTooLarge { total, cap }.into());
                }
                Ok(())
            };
            // The invitation's names and pins come from the coordinator. They
            // are matched as they are, and shown only escaped.
            let name = shown(filename, MAX_FIELD_LEN);
            let dar =
                fetch_dar_from_coordinator(config, &self.coordinator, &request, &PULL_RETRY, admit)
                    .await
                    .with_context(|| format!("DAR {index} ({name})"))?;
            // Stop at the first DAR that misses its pin, before reading the
            // rest. The whole set is checked again before any upload.
            let actual = hash_dar(&dar);
            if actual != *pinned {
                return Err(PullRefused::HashMiss {
                    filename: name,
                    actual,
                    pinned: shown(pinned, MAX_FIELD_LEN),
                }
                .into());
            }
            tracing::info!(
                "Read DAR {position}/{count} from the coordinator: {name}, {len} bytes",
                position = index + 1,
                len = dar.len()
            );
            dars.push((filename.clone(), dar));
        }
        Ok(dars)
    }
}

/// Put DARs in the order of the invitation's filenames. A pull reads them in
/// that order, and a bundle uploads in the same order. A name the invitation
/// does not hold goes last; the set check refuses it anyway.
pub fn in_invitation_order(dars: &mut [(String, Vec<u8>)], filenames: &[String]) {
    dars.sort_by_key(|(name, _)| {
        filenames
            .iter()
            .position(|filename| filename == name)
            .unwrap_or(usize::MAX)
    });
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
/// the DAR is larger than `MAX_CHUNKED_TOTAL_SIZE` ([`PullRefused::DarTooLarge`]),
/// or when the transfer fails.
pub async fn fetch_dar_from_coordinator(
    config: &NodeConfig,
    coordinator: &Peer,
    request: &RequestDarPayload,
    retry: &NoiseRetryConfig,
    admit: impl FnOnce(usize) -> Result,
) -> Result<Vec<u8>> {
    let turn = coordinator_turn(&coordinator.public_key);
    let _turn = wait_for_turn(&turn, TURN_WAIT).await?;

    let peer_pub_key = parse_public_key(&coordinator.public_key)
        .map_err(|e| anyhow::anyhow!("the coordinator's public key does not parse: {e}"))?;
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let psk = keypair.derive_psk(&peer_pub_key);
    let identity = config.participant_id().to_string();

    let message = Message::new(MessageType::RequestDar, serde_json::to_vec(request)?);
    let response = send_noise_message_within(
        &coordinator.address,
        coordinator.port,
        &psk,
        identity.as_bytes(),
        &message,
        retry,
        MAX_REPLY_SIZE,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{}", shown(&e.to_string(), MAX_REFUSAL_LEN)))?;
    let mut response = Message::from_bytes(&response)
        .map_err(|_| anyhow::anyhow!("the coordinator's answer does not parse"))?;

    match response.msg_type {
        MessageType::ChunkedCommand => {
            let (_, size, _) = parse_chunked_command_metadata(&response.payload)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if size > MAX_CHUNKED_TOTAL_SIZE {
                return Err(PullRefused::DarTooLarge {
                    filename: shown(&request.filename, MAX_FIELD_LEN),
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
            .map_err(|e| anyhow::anyhow!("{}", shown(&e.to_string(), MAX_REFUSAL_LEN)))?;
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

/// A coordinator's refusal as text for the log and the run record, escaped
/// by [`shown`] and cut at [`MAX_REFUSAL_LEN`] bytes. The peer reads and
/// decodes the whole reply, which the network read bounds at
/// [`MAX_REPLY_SIZE`]. Only the formatting here stops early: it looks at the
/// first `MAX_REFUSAL_LEN + 1` bytes of the payload.
fn refusal_text(payload: &[u8]) -> String {
    // Each byte adds at least one byte of text, so this prefix suffices.
    let prefix = &payload[..payload.len().min(MAX_REFUSAL_LEN + 1)];
    shown(&String::from_utf8_lossy(prefix), MAX_REFUSAL_LEN)
}

/// `text` as the log and the run record may show it. Only printable ASCII
/// passes as it is. Every other character is escaped, so text a coordinator
/// controls cannot break a line or reverse the text around it. The result
/// stops at `max` bytes and then ends with an ellipsis.
pub(crate) fn shown(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let printable = c == ' ' || (c.is_ascii_graphic() && c != '\\');
        let piece: String = if printable {
            c.to_string()
        } else {
            c.escape_default().collect()
        };
        if out.len() + piece.len() > max {
            out.push('…');
            break;
        }
        out.push_str(&piece);
    }
    out
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

    #[test]
    fn dars_follow_the_invitation_order() {
        let filenames = vec!["zeta.dar".to_string(), "alpha.dar".to_string()];
        let mut dars = vec![
            ("alpha.dar".to_string(), vec![1]),
            ("zeta.dar".to_string(), vec![2]),
        ];

        in_invitation_order(&mut dars, &filenames);

        let order: Vec<&str> = dars.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(order, ["zeta.dar", "alpha.dar"]);
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

    #[tokio::test]
    async fn a_fetch_stops_waiting_for_its_turn() -> anyhow::Result<()> {
        let turn = tokio::sync::Mutex::new(());
        let _held = turn.lock().await;

        let waited = tokio::time::timeout(
            Duration::from_secs(5),
            wait_for_turn(&turn, Duration::from_millis(50)),
        )
        .await?;

        let error = waited.expect_err("the turn was taken twice");
        assert!(error.to_string().contains("another DAR fetch"), "{error}");
        Ok(())
    }

    #[test]
    fn the_turn_wait_covers_a_full_pull() {
        assert_eq!(TURN_WAIT, Duration::from_secs(425));
    }

    /// `char::is_control` passes these, and they can break or reverse a
    /// line in the log and the UI.
    #[test]
    fn a_refusal_escapes_line_separators_and_direction_overrides() {
        let text = refusal_text("a\u{2028}b\u{2029}c\u{202e}d".as_bytes());

        assert_eq!(text, "a\\u{2028}b\\u{2029}c\\u{202e}d");
    }

    #[test]
    fn a_large_refusal_is_cut_without_reading_it_all() {
        let refusal = vec![b'x'; 16 * 1024 * 1024];

        let text = refusal_text(&refusal);

        assert_eq!(text.len(), MAX_REFUSAL_LEN + "…".len());
    }

    #[test]
    fn the_real_cap_plus_one_byte_reads_as_above_it() {
        let error = PullRefused::RunTooLarge {
            total: MAX_DAR_PULL_TOTAL + 1,
            cap: MAX_DAR_PULL_TOTAL,
        };

        assert!(
            error
                .to_string()
                .starts_with("the DARs of this run add up to 256.1 MiB (268435457 bytes) so far, above the cap of 256 MiB"),
            "{error}"
        );
    }

    #[test]
    fn a_long_field_is_cut_after_escaping() {
        let field = format!("{}\n", "x".repeat(MAX_FIELD_LEN));

        let text = shown(&field, MAX_FIELD_LEN);

        assert_eq!(text, format!("{}…", "x".repeat(MAX_FIELD_LEN)));
    }

    #[test]
    fn a_short_refusal_is_kept_whole() {
        assert_eq!(
            refusal_text(b"run has no DAR at index 2"),
            "run has no DAR at index 2"
        );
    }
}
