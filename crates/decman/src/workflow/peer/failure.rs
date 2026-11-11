use crate::{
    consts::{MAX_CONSECUTIVE_STEP_FAILURES, PEER_SILENCE_LIMIT},
    error::Result,
    noise::{MessageType, NoiseError, client::NoiseClient, is_transient},
    server::DeclineInvitationPayload,
    utils,
};

/// Tell the coordinator this peer gave up on the run, so it stops waiting for a
/// completion that will never come (#442).
///
/// Sent as an abandoned `DeclineInvitation`, which a coordinator that predates
/// the flag still handles by failing the run. Retried for
/// [`PEER_SILENCE_LIMIT`]: a coordinator that cannot hear from this peer for
/// that long counts it out anyway, so only a report lost to a shorter outage
/// needs the retries. A 503 means the coordinator no longer has the run.
pub(super) async fn report_departure(client: NoiseClient, report: DeclineInvitationPayload) {
    let payload = match serde_json::to_vec(&report) {
        Ok(payload) => payload,
        Err(e) => {
            tracing::warn!("Could not encode this peer's failure report: {e}");
            return;
        }
    };
    let deadline = tokio::time::Instant::now() + PEER_SILENCE_LIMIT;
    loop {
        match client.report_failure(payload.clone()).await {
            Ok(()) => return,
            Err(NoiseError::BadStatusCode(code, _)) if code.as_u16() == 503 => {
                tracing::info!("Coordinator no longer runs this workflow; nothing to report");
                return;
            }
            Err(e)
                if is_transient(&e)
                    && tokio::time::Instant::now() + FAILURE_REPORT_BACKOFF < deadline =>
            {
                tracing::warn!("Could not report this peer's failure to the coordinator: {e}");
                tokio::time::sleep(FAILURE_REPORT_BACKOFF).await;
            }
            Err(e) => {
                tracing::warn!("Gave up reporting this peer's failure to the coordinator: {e}");
                return;
            }
        }
    }
}

/// Pause between attempts to report a peer's failure.
pub(super) const FAILURE_REPORT_BACKOFF: std::time::Duration = std::time::Duration::from_secs(10);

/// Apply one retry budget to decoding, authorization, and execution failures.
/// Control messages do not reset it; only completing (or legitimately skipping)
/// a step does. The last failure returns immediately without another delay.
pub(super) async fn retry_peer_step(
    consecutive_failures: &mut usize,
    command: MessageType,
    reason: String,
) -> Result {
    *consecutive_failures += 1;
    // The retry below then resolves the successor synchronizer after an LSU.
    utils::forget_synchronizer_id_on_unknown_store(&reason);
    tracing::warn!(
        ?command,
        attempt = *consecutive_failures,
        error = %reason,
        "Peer step failed"
    );
    if *consecutive_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
        anyhow::bail!(
            "Aborting peer after {MAX_CONSECUTIVE_STEP_FAILURES} consecutive step failures \
             ({command:?}): {reason}"
        );
    }
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    Ok(())
}
