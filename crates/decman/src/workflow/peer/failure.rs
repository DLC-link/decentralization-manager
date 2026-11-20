use std::time::Duration;

use tokio::time::Instant;

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
    let deadline = Instant::now() + PEER_SILENCE_LIMIT;
    loop {
        match client.report_failure(payload.clone()).await {
            Ok(()) => return,
            Err(NoiseError::BadStatusCode(code, _)) if code.as_u16() == 503 => {
                tracing::info!("Coordinator no longer runs this workflow; nothing to report");
                return;
            }
            Err(e) if is_transient(&e) && Instant::now() + FAILURE_REPORT_BACKOFF < deadline => {
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
pub(super) const FAILURE_REPORT_BACKOFF: Duration = Duration::from_secs(10);

// Five pauses give uploads a sixty-second recovery window before the sixth attempt.
const SIGNATURE_UPLOAD_BACKOFF: Duration = Duration::from_secs(12);

const PEER_RETRY_BACKOFF: Duration = Duration::from_secs(2);

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
    tokio::time::sleep(PEER_RETRY_BACKOFF).await;
    Ok(())
}

/// Upload failures have their own budget: successful polling or signing cannot reset it.
pub(super) async fn record_signature_upload(
    consecutive_failures: &mut usize,
    command: MessageType,
    outcome: Result,
) -> Result {
    let Err(error) = outcome else {
        *consecutive_failures = 0;
        return Ok(());
    };
    *consecutive_failures += 1;
    tracing::warn!(
        ?command,
        attempt = *consecutive_failures,
        error = %error,
        "Signature upload failed"
    );
    if *consecutive_failures >= MAX_CONSECUTIVE_STEP_FAILURES {
        anyhow::bail!(
            "Peer failed: signature upload failed after {} consecutive attempts ({command:?}): {error}",
            *consecutive_failures
        );
    }
    tracing::warn!(
        ?command,
        attempt = *consecutive_failures,
        backoff_seconds = SIGNATURE_UPLOAD_BACKOFF.as_secs(),
        "Retrying signature upload"
    );
    tokio::time::sleep(SIGNATURE_UPLOAD_BACKOFF).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn signature_upload_recovers_and_resets_budget() {
        let mut failures = 0;
        let start = Instant::now();
        record_signature_upload(
            &mut failures,
            MessageType::SignAddParty,
            Err(anyhow::anyhow!("temporary storage outage")),
        )
        .await
        .unwrap();
        assert_eq!(failures, 1);
        assert!(start.elapsed() >= SIGNATURE_UPLOAD_BACKOFF);
        record_signature_upload(&mut failures, MessageType::SignAddParty, Ok(()))
            .await
            .unwrap();
        assert_eq!(failures, 0);
        record_signature_upload(
            &mut failures,
            MessageType::SignAddParty,
            Err(anyhow::anyhow!("another outage")),
        )
        .await
        .unwrap();
        assert_eq!(failures, 1);
    }

    #[tokio::test]
    async fn signature_upload_exhaustion_reports_cause() {
        let mut failures = 0;
        let start = Instant::now();
        for attempt in 1..MAX_CONSECUTIVE_STEP_FAILURES {
            record_signature_upload(
                &mut failures,
                MessageType::SignChangeThreshold,
                Err(anyhow::anyhow!("local artifact missing")),
            )
            .await
            .unwrap();
            assert_eq!(failures, attempt);
        }
        let error = record_signature_upload(
            &mut failures,
            MessageType::SignChangeThreshold,
            Err(anyhow::anyhow!("local artifact missing")),
        )
        .await
        .unwrap_err();
        assert_eq!(failures, MAX_CONSECUTIVE_STEP_FAILURES);
        assert!(start.elapsed() >= Duration::from_secs(60));
        let report = format!("{error:#}");
        assert!(report.contains("SignChangeThreshold"));
        assert!(report.contains(&failures.to_string()));
        assert!(report.contains("local artifact missing"));
    }
}
