//! A peer reads each DAR of a Dars run from a scripted coordinator.

use std::time::Duration;

use anyhow::Context;

use super::peer::{DarPull, OversizedPull, fetch_dar_from_coordinator};
use crate::{
    consts::MAX_CONSECUTIVE_STEP_FAILURES,
    error::Result,
    noise::{CHUNK_SIZE, MAX_CHUNKED_TOTAL_SIZE, MAX_PAYLOAD_SIZE, Message, MessageType},
    server::{RequestDarPayload, WorkflowKind},
    utils,
    workflow::{
        peer_retry_tests::{
            COORDINATOR_RUN, DarServer, PeerRun, Script, assert_reported, chunked_meta, run_script,
        },
        validation::hash_dar,
    },
};

/// DARs of the given sizes. Their index order is not their name order, so a
/// test can tell which order the peer used.
fn dars(sizes: &[usize]) -> Vec<(String, Vec<u8>)> {
    ["zeta", "alpha", "mid"]
        .iter()
        .zip(sizes)
        .zip(1u8..)
        .map(|((name, size), fill)| (format!("{name}.dar"), vec![fill; *size]))
        .collect()
}

/// A run that offers `dars`, with an invitation that names and pins each one.
fn invited(dars: &[(String, Vec<u8>)]) -> Script {
    Script {
        dar_filenames: dars.iter().map(|(name, _)| name.clone()).collect(),
        dar_hashes: dars.iter().map(|(_, dar)| hash_dar(dar)).collect(),
        dars: dars.to_vec(),
        ..Script::default()
    }
}

/// The `UploadDars` command for a bundle of `size` bytes, too large for one
/// frame.
fn chunked_upload(size: usize) -> Result<Message> {
    Ok(Message::new(
        MessageType::ChunkedCommand,
        chunked_meta(MessageType::UploadDars, size)?,
    ))
}

/// What the participant records for each DAR: its name without `.dar`.
fn uploaded(dars: &[(String, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
    dars.iter()
        .map(|(name, dar)| (name.trim_end_matches(".dar").to_string(), dar.clone()))
        .collect()
}

fn requested(run: &PeerRun) -> Vec<(String, usize, String)> {
    run.dar_requests
        .iter()
        .map(|r| (r.workflow_instance.clone(), r.index, r.filename.clone()))
        .collect()
}

fn error_of(run: &PeerRun) -> Result<String> {
    run.result
        .as_ref()
        .err()
        .map(|e| format!("{e:#}"))
        .ok_or_else(|| anyhow::anyhow!("the peer run did not fail"))
}

#[tokio::test]
async fn the_peer_reads_each_dar_by_index_and_leaves_the_bundle() -> Result {
    // The middle DAR is too large for one frame, so it arrives in chunks.
    let dars = dars(&[100, MAX_PAYLOAD_SIZE + 1, 200]);
    let bundle = utils::encode_files(&dars);
    let mut script = invited(&dars);
    script.commands = vec![chunked_upload(bundle.len())?];
    script.command_payload = bundle;

    let run = run_script(WorkflowKind::Dars, script).await?;

    assert!(run.result.is_ok(), "{:?}", run.result);
    let expected: Vec<_> = dars
        .iter()
        .enumerate()
        .map(|(index, (name, _))| (COORDINATOR_RUN.to_string(), index, name.clone()))
        .collect();
    assert_eq!(requested(&run), expected);
    assert_eq!(run.command_chunks, 0, "the peer read the bundle");
    assert_eq!(run.uploads, uploaded(&dars), "not uploaded in index order");
    assert_eq!(run.completions, 1);
    Ok(())
}

/// Issue #616: the chunked limit applies to each DAR, not to the set.
#[tokio::test]
async fn a_set_above_the_chunked_limit_uploads_when_each_dar_fits() -> Result {
    let each = MAX_CHUNKED_TOTAL_SIZE / 3 + 1;
    let dars = dars(&[each, each, each]);
    let bundle_size = utils::encode_files(&dars).len();
    assert!(bundle_size > MAX_CHUNKED_TOTAL_SIZE);
    let mut script = invited(&dars);
    script.commands = vec![chunked_upload(bundle_size)?];

    let run = run_script(WorkflowKind::Dars, script).await?;

    assert!(run.result.is_ok(), "{:?}", run.result);
    assert_eq!(run.uploads, uploaded(&dars));
    assert_eq!(run.command_chunks, 0, "the peer read the bundle");
    assert_eq!(run.completions, 1);
    Ok(())
}

/// Each DAR is checked against its pin as it arrives. The peer stops at the
/// first miss and reads none of the DARs after it.
#[tokio::test]
async fn a_dar_that_misses_its_hash_stops_every_upload() -> Result {
    let dars = dars(&[100, 200, 300]);
    let mut script = invited(&dars);
    // The coordinator serves other bytes under the middle name.
    script.dars[1].1 = b"not the accepted DAR".to_vec();
    script.commands = vec![chunked_upload(MAX_PAYLOAD_SIZE + 1)?; MAX_CONSECUTIVE_STEP_FAILURES];

    let run = run_script(WorkflowKind::Dars, script).await?;

    let error = error_of(&run)?;
    assert!(error.contains("DAR alpha.dar hashes to"), "{error}");
    assert_eq!(
        run.dar_requests.len(),
        2 * MAX_CONSECUTIVE_STEP_FAILURES,
        "the peer read past the DAR that missed its pin"
    );
    assert!(run.uploads.is_empty(), "uploaded before the check");
    assert_eq!(run.completions, 0);
    assert_reported(&run, WorkflowKind::Dars);
    Ok(())
}

#[tokio::test]
async fn a_dar_the_coordinator_does_not_have_stops_every_upload() -> Result {
    let dars = dars(&[100, 200, 300]);
    let mut script = invited(&dars);
    script.dars.truncate(2);
    // A coordinator that sends no bundle at all is read the same way.
    script.commands =
        vec![Message::new_empty(MessageType::UploadDars); MAX_CONSECUTIVE_STEP_FAILURES];

    let run = run_script(WorkflowKind::Dars, script).await?;

    let error = error_of(&run)?;
    assert!(
        error.contains("DAR 2 (mid.dar): run has no DAR at index 2"),
        "{error}"
    );
    assert!(run.uploads.is_empty(), "uploaded part of the set");
    assert_eq!(run.completions, 0);
    assert_reported(&run, WorkflowKind::Dars);
    Ok(())
}

#[tokio::test]
async fn a_dar_above_the_chunked_limit_is_refused_by_name() -> Result {
    let dars = dars(&[100, MAX_CHUNKED_TOTAL_SIZE + 1]);
    let mut script = invited(&dars);
    script.commands =
        vec![chunked_upload(MAX_CHUNKED_TOTAL_SIZE + 200)?; MAX_CONSECUTIVE_STEP_FAILURES];

    let run = run_script(WorkflowKind::Dars, script).await?;

    let error = error_of(&run)?;
    let expected = format!(
        "alpha.dar is {} bytes, above the limit of {MAX_CHUNKED_TOTAL_SIZE} bytes for one DAR",
        MAX_CHUNKED_TOTAL_SIZE + 1
    );
    assert!(error.contains(&expected), "{error}");
    // The size is the same on every attempt, so the run ends at once.
    assert_eq!(run.polls.len(), 1, "the peer retried");
    assert_eq!(run.dar_requests.len(), 2, "the peer retried");
    assert!(run.uploads.is_empty());
    assert_reported(&run, WorkflowKind::Dars);
    Ok(())
}

#[tokio::test]
async fn an_invitation_that_names_a_dar_twice_is_refused_before_any_pull() -> Result {
    let dars = dars(&[100, 200]);
    let mut script = invited(&dars);
    script.dar_filenames[1] = script.dar_filenames[0].clone();
    script.commands = vec![chunked_upload(MAX_PAYLOAD_SIZE + 1)?; MAX_CONSECUTIVE_STEP_FAILURES];

    let run = run_script(WorkflowKind::Dars, script).await?;

    let error = error_of(&run)?;
    assert!(error.contains("names the same DAR twice"), "{error}");
    assert!(run.dar_requests.is_empty(), "the peer pulled a DAR");
    assert!(run.uploads.is_empty());
    assert_reported(&run, WorkflowKind::Dars);
    Ok(())
}

/// A coordinator that leaves the bundle behind expects the peer to pull. A
/// peer whose invitation does not allow a pull says why.
#[tokio::test]
async fn an_upload_without_dars_that_the_peer_cannot_pull_names_the_cause() -> Result {
    let dars = dars(&[100, 200]);
    let mut script = invited(&dars);
    script.dar_hashes.clear();
    script.commands =
        vec![Message::new_empty(MessageType::UploadDars); MAX_CONSECUTIVE_STEP_FAILURES];

    let run = run_script(WorkflowKind::Dars, script).await?;

    let error = error_of(&run)?;
    assert!(
        error.contains(
            "a pull needs the coordinator's run instance and one hash for each DAR in the \
             invitation"
        ),
        "{error}"
    );
    assert!(run.dar_requests.is_empty());
    assert!(run.uploads.is_empty());
    assert_reported(&run, WorkflowKind::Dars);
    Ok(())
}

/// The announced sizes count toward the run's cap before the DAR that
/// crosses it is read.
#[tokio::test]
async fn a_run_above_the_total_cap_is_refused_at_the_dar_that_crosses_it() -> Result {
    let dars = dars(&[MAX_PAYLOAD_SIZE + 1, MAX_PAYLOAD_SIZE + 1, 100]);
    let server = DarServer::start(dars.clone(), Duration::ZERO).await?;
    let names: Vec<String> = dars.iter().map(|(name, _)| name.clone()).collect();
    let hashes: Vec<String> = dars.iter().map(|(_, dar)| hash_dar(dar)).collect();
    let pull = DarPull::new(&server.coordinator, COORDINATOR_RUN, &names, &hashes)?
        .context("no pull planned")?;
    let cap = 3 * MAX_PAYLOAD_SIZE / 2;

    let error = pull
        .fetch_within(&server.node, cap)
        .await
        .expect_err("a run above the cap was pulled");

    assert!(error.downcast_ref::<OversizedPull>().is_some(), "{error:#}");
    let expected = format!(
        "the DARs of this run add up to {} bytes so far, above the cap of {cap} bytes",
        2 * (MAX_PAYLOAD_SIZE + 1)
    );
    assert!(format!("{error:#}").contains(&expected), "{error:#}");
    assert_eq!(server.requests()?.len(), 2, "the peer read past the cap");
    Ok(())
}

/// The coordinator keeps one chunked `RequestDar` answer for each peer. Two
/// fetches at once, such as a run's pull and an invitation preview, would
/// read each other's chunks. They take turns instead.
#[tokio::test]
async fn two_fetches_from_one_coordinator_take_turns() -> Result {
    let dars = dars(&[MAX_PAYLOAD_SIZE + 1, MAX_PAYLOAD_SIZE + 2]);
    let server = DarServer::start(dars.clone(), Duration::from_millis(300)).await?;
    let fetch = |index: usize| {
        let request = RequestDarPayload {
            workflow_instance: COORDINATOR_RUN.to_string(),
            index,
            filename: dars[index].0.clone(),
        };
        let server = &server;
        async move {
            fetch_dar_from_coordinator(
                &server.node,
                &server.coordinator,
                &request,
                &server.node.noise_retry,
                |_| Ok(()),
            )
            .await
        }
    };

    let (first, second) = tokio::join!(fetch(0), fetch(1));

    assert!(first? == dars[0].1, "the first fetch read other bytes");
    assert!(second? == dars[1].1, "the second fetch read other bytes");
    Ok(())
}

/// A coordinator that predates the hashes gets the bundle path.
#[tokio::test]
async fn an_invitation_without_hashes_reads_the_bundle() -> Result {
    let dars = dars(&[100, MAX_PAYLOAD_SIZE, 200]);
    let bundle = utils::encode_files(&dars);
    let mut script = invited(&dars);
    script.dar_hashes.clear();
    script.commands = vec![chunked_upload(bundle.len())?];
    let chunks = bundle.len().div_ceil(CHUNK_SIZE);
    script.command_payload = bundle;

    let run = run_script(WorkflowKind::Dars, script).await?;

    assert!(run.result.is_ok(), "{:?}", run.result);
    assert!(
        run.dar_requests.is_empty(),
        "the peer asked for single DARs"
    );
    assert_eq!(run.command_chunks, chunks);
    assert_eq!(run.uploads, uploaded(&dars));
    assert_eq!(run.completions, 1);
    Ok(())
}

#[tokio::test]
async fn an_inline_bundle_is_used_as_sent() -> Result {
    let dars = dars(&[100, 200]);
    let mut script = invited(&dars);
    script.commands = vec![Message::new(
        MessageType::UploadDars,
        utils::encode_files(&dars),
    )];

    let run = run_script(WorkflowKind::Dars, script).await?;

    assert!(run.result.is_ok(), "{:?}", run.result);
    assert!(
        run.dar_requests.is_empty(),
        "the peer asked for single DARs"
    );
    assert_eq!(run.uploads, uploaded(&dars));
    assert_eq!(run.completions, 1);
    Ok(())
}

/// The limit is inclusive: a DAR of exactly the chunked limit fits one
/// transfer and is uploaded.
#[tokio::test]
async fn a_dar_at_the_chunked_limit_is_pulled() -> Result {
    let dars = dars(&[100, MAX_CHUNKED_TOTAL_SIZE]);
    let mut script = invited(&dars);
    script.commands = vec![chunked_upload(MAX_CHUNKED_TOTAL_SIZE + 200)?];

    let run = run_script(WorkflowKind::Dars, script).await?;

    assert!(run.result.is_ok(), "{:?}", run.result);
    assert_eq!(run.uploads, uploaded(&dars));
    assert_eq!(run.completions, 1);
    Ok(())
}

/// Issue #616 reports a run of 21 DARs, 26.3 MB in total, that failed on
/// the peer. Each DAR here is above one frame, so each arrives in chunks,
/// and the set is far above the chunked limit.
#[tokio::test]
async fn the_reported_set_of_twenty_one_dars_uploads_in_index_order() -> Result {
    let each = MAX_PAYLOAD_SIZE + MAX_PAYLOAD_SIZE / 4;
    let dars: Vec<(String, Vec<u8>)> = (0..21u8)
        .map(|i| (format!("pkg-{:02}.dar", 20 - i), vec![i; each]))
        .collect();
    let bundle_size = utils::encode_files(&dars).len();
    assert!(bundle_size > MAX_CHUNKED_TOTAL_SIZE);
    let mut script = invited(&dars);
    script.commands = vec![chunked_upload(bundle_size)?];

    let run = run_script(WorkflowKind::Dars, script).await?;

    assert!(run.result.is_ok(), "{:?}", run.result);
    let expected: Vec<_> = dars
        .iter()
        .enumerate()
        .map(|(index, (name, _))| (COORDINATOR_RUN.to_string(), index, name.clone()))
        .collect();
    assert_eq!(requested(&run), expected);
    assert_eq!(run.command_chunks, 0, "the peer read the bundle");
    assert_eq!(run.uploads, uploaded(&dars), "not uploaded in index order");
    assert_eq!(run.completions, 1);
    Ok(())
}
