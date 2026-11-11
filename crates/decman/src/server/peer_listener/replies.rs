//! DAR and chunked replies to peers.

use std::time::Instant;

use anyhow::{Context, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use hyper::{Body, Response, StatusCode};
use sqlx::SqlitePool;

use crate::server::state::{PEER_CHUNK_CACHE_TTL, PeerChunkCache};
use crate::server::types::{RequestDarPayload, WorkflowKind};
use crate::{
    db::schema::SchemaRead,
    error::Result,
    noise::{CHUNK_SIZE, MAX_CHUNKED_TOTAL_SIZE, MAX_PAYLOAD_SIZE, Message, MessageType},
    workflow,
};

/// Read one DAR of a Dars run for a peer that run invited, so its operator can
/// look at the DAR before accepting. Everything is checked against the run the
/// coordinator actually started: an uninvited peer, another kind of run, or a
/// name that no longer matches the offered index gets nothing.
pub(super) async fn dar_for_invited_peer(
    db: &SqlitePool,
    sender: Option<&str>,
    payload: &[u8],
) -> Result<Vec<u8>> {
    let request: RequestDarPayload =
        serde_json::from_slice(payload).context("malformed RequestDar payload")?;
    let sender = sender.context("the caller is not a known peer")?;

    let run = db
        .get_workflow_run(&request.workflow_instance)
        .await?
        .with_context(|| format!("no run named {}", request.workflow_instance))?;
    if run.kind != WorkflowKind::Dars {
        bail!("run {} does not distribute DARs", request.workflow_instance);
    }
    if !run.expected_peers.iter().any(|p| p.to_string() == sender) {
        bail!("{sender} was not invited to {}", request.workflow_instance);
    }

    let config: workflow::DarsConfig = serde_json::from_str(&run.config_json)
        .with_context(|| format!("unreadable config for {}", request.workflow_instance))?;
    let file = config
        .dar_files
        .get(request.index)
        .with_context(|| format!("run has no DAR at index {}", request.index))?;
    if file.filename != request.filename {
        bail!(
            "DAR {} of this run is {}, not {}",
            request.index,
            file.filename,
            request.filename
        );
    }
    STANDARD
        .decode(&file.data)
        .with_context(|| format!("DAR {} does not decode", file.filename))
}

/// Answer a peer request with `payload`: inline while it fits one Noise frame,
/// otherwise cached per peer and announced as a chunked transfer the peer pulls
/// with `GetChunk`. `what` names the payload in the logs.
pub(super) async fn peer_payload_response(
    payload: Vec<u8>,
    peer_pubkey_hex: Option<&str>,
    cache: &PeerChunkCache,
    what: &str,
) -> Response<Body> {
    let inline = |payload: Vec<u8>| {
        Response::new(Body::from(
            Message::new(MessageType::Data, payload).to_bytes(),
        ))
    };

    if payload.len() <= MAX_PAYLOAD_SIZE {
        return inline(payload);
    }

    // Without a peer pubkey there is nowhere to key the cache. Send it whole
    // anyway: it will fail at the transport, but the log says why.
    let Some(pk) = peer_pubkey_hex else {
        tracing::warn!(
            "{what} is {} bytes (> {} chunk threshold) but the peer is \
             unidentified; cannot chunk. Sending unchunked anyway, may fail at \
             transport.",
            payload.len(),
            MAX_PAYLOAD_SIZE,
        );
        return inline(payload);
    };

    // Symmetric with the client's `MAX_CHUNKED_TOTAL_SIZE`: keeps the per-peer
    // cache bounded and the `usize → u32` casts below honest.
    if payload.len() > MAX_CHUNKED_TOTAL_SIZE {
        tracing::error!(
            "{what} is {} bytes; exceeds chunked cap {} — refusing to chunk",
            payload.len(),
            MAX_CHUNKED_TOTAL_SIZE,
        );
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        return response;
    }

    // Casts are infallible against the cap checked just above.
    let total_size = payload.len() as u32;
    let chunk_count = payload.len().div_ceil(CHUNK_SIZE) as u32;
    tracing::info!(
        "{what} too large ({total_size} bytes), chunking into {chunk_count} \
         chunks for peer {pk}",
    );

    {
        let mut cache = cache.lock().await;
        cache.retain(|_, (_, t)| t.elapsed() < PEER_CHUNK_CACHE_TTL);
        cache.insert(pk.to_owned(), (payload, Instant::now()));
    }

    // ChunkedCommand metadata: [Data:2][total_size:4][chunk_count:4]. The type
    // is what the client reconstitutes the assembled payload as.
    let mut meta = Vec::with_capacity(10);
    meta.extend_from_slice(&MessageType::Data.to_u16().to_be_bytes());
    meta.extend_from_slice(&total_size.to_be_bytes());
    meta.extend_from_slice(&chunk_count.to_be_bytes());

    Response::new(Body::from(
        Message::new(MessageType::ChunkedCommand, meta).to_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    mod serving_a_dar_to_a_peer {
        use super::super::dar_for_invited_peer;
        use crate::{
            canton_id::CantonId,
            db::{
                MIGRATOR,
                schema::{Commitable, SchemaWrite},
            },
            server::types::{
                RequestDarPayload, WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun,
            },
            workflow::DarsConfig,
        };
        use base64::{Engine, engine::general_purpose::STANDARD};
        use sqlx::SqlitePool;

        const DAR: &[u8] = b"not really a DAR, but bytes are bytes";

        fn pid(prefix: &str, tag: u8) -> anyhow::Result<CantonId> {
            CantonId::parse(&format!(
                "{prefix}::1220{}",
                format!("{tag:02x}").repeat(32)
            ))
            .map_err(|e| anyhow::anyhow!("bad test id {prefix}: {e}"))
        }

        fn dars_run(instance: &str, invited: Vec<CantonId>) -> anyhow::Result<WorkflowRun> {
            let config = DarsConfig {
                dar_files: vec![common::api::DarFile {
                    filename: "app.dar".to_string(),
                    data: STANDARD.encode(DAR),
                }],
                peer_ids: invited.clone(),
                instance_name: instance.to_string(),
            };
            Ok(WorkflowRun {
                instance_name: instance.to_string(),
                kind: WorkflowKind::Dars,
                role: WorkflowRole::Coordinator,
                status: WorkflowProgress::InProgress,
                current_step: "WaitingForPeers".to_string(),
                step_index: 0,
                step_total: 3,
                config_json: serde_json::to_string(&config)?,
                coordinator_pubkey: None,
                coordinator_instance: None,
                coordinator_name: None,
                expected_peers: invited,
                completed_peers: Vec::new(),
                connected_peers: Vec::new(),
                acs_progress: None,
                dec_party_id: None,
                prefix: None,
                participants: Vec::new(),
                previous_threshold: None,
                new_threshold: None,
                kicked_participant: None,
                kicked_owner_key: None,
                added_participant: None,
                package_names: Vec::new(),
                dar_filenames: Vec::new(),
                error: None,
                dismissed: false,
                created_at: 0,
                updated_at: 0,
            })
        }

        async fn save(pool: &SqlitePool, run: &WorkflowRun) -> anyhow::Result<()> {
            let mut tx = pool.begin_transaction().await?;
            tx.upsert_workflow_run(run).await?;
            Commitable::commit(tx).await?;
            Ok(())
        }

        fn ask(instance: &str, index: usize, filename: &str) -> anyhow::Result<Vec<u8>> {
            Ok(serde_json::to_vec(&RequestDarPayload {
                workflow_instance: instance.to_string(),
                index,
                filename: filename.to_string(),
            })?)
        }

        #[sqlx::test(migrator = "MIGRATOR")]
        async fn an_invited_peer_reads_the_dar(pool: SqlitePool) -> anyhow::Result<()> {
            let invited = pid("validator-1", 0xaa)?;
            save(&pool, &dars_run("dars-1", vec![invited.clone()])?).await?;

            let served = dar_for_invited_peer(
                &pool,
                Some(&invited.to_string()),
                &ask("dars-1", 0, "app.dar")?,
            )
            .await?;

            assert_eq!(served, DAR);
            Ok(())
        }

        /// The gate this endpoint exists for: the DAR is readable before the
        /// peer accepts, so nothing but the invite list says who may read it.
        #[sqlx::test(migrator = "MIGRATOR")]
        async fn an_uninvited_peer_gets_nothing(pool: SqlitePool) -> anyhow::Result<()> {
            let invited = pid("validator-1", 0xaa)?;
            let stranger = pid("validator-9", 0xbb)?;
            save(&pool, &dars_run("dars-1", vec![invited])?).await?;

            match dar_for_invited_peer(
                &pool,
                Some(&stranger.to_string()),
                &ask("dars-1", 0, "app.dar")?,
            )
            .await
            {
                Ok(_) => panic!("a stranger must not read the DAR"),
                Err(e) => assert!(e.to_string().contains("was not invited"), "{e}"),
            }
            Ok(())
        }

        #[sqlx::test(migrator = "MIGRATOR")]
        async fn an_unidentified_caller_gets_nothing(pool: SqlitePool) -> anyhow::Result<()> {
            let invited = pid("validator-1", 0xaa)?;
            save(&pool, &dars_run("dars-1", vec![invited])?).await?;

            let refused = dar_for_invited_peer(&pool, None, &ask("dars-1", 0, "app.dar")?).await;

            assert!(refused.is_err(), "an unidentified caller must not read it");
            Ok(())
        }

        /// The index picks the DAR and the name comes from the same invite, so
        /// a request where they disagree is stale or forged either way.
        #[sqlx::test(migrator = "MIGRATOR")]
        async fn a_name_that_does_not_match_the_index_is_refused(
            pool: SqlitePool,
        ) -> anyhow::Result<()> {
            let invited = pid("validator-1", 0xaa)?;
            save(&pool, &dars_run("dars-1", vec![invited.clone()])?).await?;

            match dar_for_invited_peer(
                &pool,
                Some(&invited.to_string()),
                &ask("dars-1", 0, "something-else.dar")?,
            )
            .await
            {
                Ok(_) => panic!("a name that does not match the index must be refused"),
                Err(e) => assert!(e.to_string().contains("something-else.dar"), "{e}"),
            }
            Ok(())
        }

        #[sqlx::test(migrator = "MIGRATOR")]
        async fn an_index_past_the_end_is_refused(pool: SqlitePool) -> anyhow::Result<()> {
            let invited = pid("validator-1", 0xaa)?;
            save(&pool, &dars_run("dars-1", vec![invited.clone()])?).await?;

            let refused = dar_for_invited_peer(
                &pool,
                Some(&invited.to_string()),
                &ask("dars-1", 7, "app.dar")?,
            )
            .await;

            assert!(refused.is_err(), "there is no DAR at index 7");
            Ok(())
        }
    }
}
