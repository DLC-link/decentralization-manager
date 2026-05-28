use std::time::{Duration, Instant};

use reqwest::StatusCode;
use secp256k1::PublicKey;

use crate::consts::PROBE_REQUEST_TIMEOUT;
use crate::noise::{NoiseKeypair, probe_sig::sign_probe};
use crate::server::{PeerProbeResponse, WorkflowKind, WorkflowProgress};

#[derive(Debug)]
pub enum CoordinatorState {
    InProgress,
    Cancelled,
    Completed,
    Failed { error: Option<String> },
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("no coordinator_http_url on this peer's row")]
    NoUrl,
    #[error("coordinator has no record of this run")]
    NotFound,
    #[error("probe HTTP transport: {0}")]
    Transport(String),
    #[error("probe HTTP timeout after {0:?}")]
    Timeout(Duration),
    #[error("probe verification failed: {0}")]
    Auth(String),
}

pub async fn probe_coordinator_state(
    base_url: Option<&str>,
    keypair: &NoiseKeypair,
    coord_pub: &PublicKey,
    kind: WorkflowKind,
) -> std::result::Result<CoordinatorState, ProbeError> {
    let base = base_url.ok_or(ProbeError::NoUrl)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let kind_str: &str = match kind {
        WorkflowKind::Onboarding => "Onboarding",
        WorkflowKind::Kick => "Kick",
        WorkflowKind::Contracts => "Contracts",
        WorkflowKind::Dars => "Dars",
    };
    let sig = sign_probe(keypair, coord_pub, kind_str, ts);
    let sig_hex = hex::encode(sig);
    let peer_pubkey_hex = keypair.public_key_hex();

    let client = reqwest::Client::builder()
        .timeout(PROBE_REQUEST_TIMEOUT)
        .build()
        .map_err(|e| ProbeError::Transport(e.to_string()))?;

    let url = format!("{base}/workflows/peer-probe");
    let resp = match client
        .get(&url)
        .query(&[("kind", kind_str)])
        .header("X-Probe-Peer-Pubkey", peer_pubkey_hex)
        .header("X-Probe-Timestamp", ts.to_string())
        .header("X-Probe-Signature", sig_hex)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_timeout() => return Err(ProbeError::Timeout(PROBE_REQUEST_TIMEOUT)),
        Err(e) => return Err(ProbeError::Transport(e.to_string())),
    };

    match resp.status() {
        StatusCode::NOT_FOUND => return Err(ProbeError::NotFound),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err(ProbeError::Auth(resp.status().to_string()));
        }
        s if !s.is_success() => return Err(ProbeError::Transport(format!("HTTP {s}"))),
        _ => {}
    }
    let body: PeerProbeResponse = resp
        .json()
        .await
        .map_err(|e| ProbeError::Transport(format!("decode: {e}")))?;
    Ok(match body.status {
        WorkflowProgress::InProgress => CoordinatorState::InProgress,
        WorkflowProgress::Cancelled => CoordinatorState::Cancelled,
        WorkflowProgress::Completed => CoordinatorState::Completed,
        WorkflowProgress::Failed => CoordinatorState::Failed { error: body.error },
        // Idle means the coordinator has no active run for this peer yet;
        // treat as still-pending (not yet in-progress) → tolerate.
        WorkflowProgress::Idle => CoordinatorState::InProgress,
    })
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[tokio::test]
    async fn no_url_returns_no_url_error() {
        let kp = NoiseKeypair::generate();
        let coord_kp = NoiseKeypair::generate();
        let err =
            probe_coordinator_state(None, &kp, &coord_kp.public_key, WorkflowKind::Onboarding)
                .await
                .unwrap_err();
        assert!(matches!(err, ProbeError::NoUrl));
    }

    // Other cases are exercised end-to-end in the handler tests (Task 11) +
    // the integration test (Tasks 15-16). A targeted unit test for transport-
    // error/timeout would need a mock HTTP server (e.g. wiremock) — flagged
    // as a follow-up; not blocking the fix.
}

#[derive(Debug, PartialEq)]
pub enum BudgetState {
    Tolerate,
    Expired,
}

pub struct BudgetTracker {
    first_failure_at: Option<Instant>,
    budget: Duration,
}

impl BudgetTracker {
    pub fn new(budget: Duration) -> Self {
        Self {
            first_failure_at: None,
            budget,
        }
    }

    pub fn record_failure(&mut self) -> BudgetState {
        let now = Instant::now();
        match self.first_failure_at {
            None => {
                self.first_failure_at = Some(now);
                BudgetState::Tolerate
            }
            Some(t0) if now.duration_since(t0) > self.budget => BudgetState::Expired,
            Some(_) => BudgetState::Tolerate,
        }
    }

    pub fn reset(&mut self) {
        self.first_failure_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_failure_tolerates() {
        let mut t = BudgetTracker::new(Duration::from_secs(180));
        assert_eq!(t.record_failure(), BudgetState::Tolerate);
    }

    #[test]
    fn expires_after_budget() {
        let mut t = BudgetTracker::new(Duration::from_micros(1));
        assert_eq!(t.record_failure(), BudgetState::Tolerate);
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(t.record_failure(), BudgetState::Expired);
    }

    #[test]
    fn reset_clears_first_failure() {
        let mut t = BudgetTracker::new(Duration::from_micros(1));
        let _ = t.record_failure();
        std::thread::sleep(Duration::from_millis(2));
        t.reset();
        assert_eq!(t.record_failure(), BudgetState::Tolerate);
    }
}

/// Action the peer command-poll should take on Noise failure, given the
/// probe outcome and the current budget state. Pure — no I/O, no DB writes.
#[derive(Debug, PartialEq)]
pub enum PeerAction {
    BailCancelled,
    Tolerate,
    BailFailedCompletedWithoutMe,
    BailFailedFromCoordinator(Option<String>),
    BailFailedNotFound,
    BudgetState(BudgetState),
    LegacyFallback,
}

pub fn decide_action(
    probe: std::result::Result<CoordinatorState, ProbeError>,
    budget: &mut BudgetTracker,
) -> PeerAction {
    match probe {
        Ok(CoordinatorState::Cancelled) => PeerAction::BailCancelled,
        Ok(CoordinatorState::InProgress) => {
            budget.reset();
            PeerAction::Tolerate
        }
        Ok(CoordinatorState::Completed) => PeerAction::BailFailedCompletedWithoutMe,
        Ok(CoordinatorState::Failed { error }) => PeerAction::BailFailedFromCoordinator(error),
        Err(ProbeError::NotFound) => PeerAction::BailFailedNotFound,
        Err(ProbeError::NoUrl) => PeerAction::LegacyFallback,
        Err(_) => PeerAction::BudgetState(budget.record_failure()),
    }
}

#[cfg(test)]
mod decide_action_tests {
    use super::*;

    fn fresh() -> BudgetTracker {
        BudgetTracker::new(Duration::from_secs(180))
    }

    #[test]
    fn cancelled_bails() {
        let mut b = fresh();
        assert_eq!(
            decide_action(Ok(CoordinatorState::Cancelled), &mut b),
            PeerAction::BailCancelled
        );
    }

    #[test]
    fn in_progress_resets_budget() {
        let mut b = BudgetTracker::new(Duration::from_micros(1));
        let _ = b.record_failure();
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(
            decide_action(Ok(CoordinatorState::InProgress), &mut b),
            PeerAction::Tolerate
        );
        assert_eq!(
            decide_action(Err(ProbeError::Transport("x".into())), &mut b),
            PeerAction::BudgetState(BudgetState::Tolerate)
        );
    }

    #[test]
    fn completed_bails_failed() {
        let mut b = fresh();
        assert_eq!(
            decide_action(Ok(CoordinatorState::Completed), &mut b),
            PeerAction::BailFailedCompletedWithoutMe
        );
    }

    #[test]
    fn coord_failed_propagates() {
        let mut b = fresh();
        assert_eq!(
            decide_action(
                Ok(CoordinatorState::Failed {
                    error: Some("x".into())
                }),
                &mut b
            ),
            PeerAction::BailFailedFromCoordinator(Some("x".into()))
        );
    }

    #[test]
    fn not_found_bails_failed() {
        let mut b = fresh();
        assert_eq!(
            decide_action(Err(ProbeError::NotFound), &mut b),
            PeerAction::BailFailedNotFound
        );
    }

    #[test]
    fn no_url_triggers_legacy_fallback() {
        let mut b = fresh();
        assert_eq!(
            decide_action(Err(ProbeError::NoUrl), &mut b),
            PeerAction::LegacyFallback
        );
    }

    #[test]
    fn transport_accrues_then_expires() {
        let mut b = BudgetTracker::new(Duration::from_micros(1));
        assert_eq!(
            decide_action(Err(ProbeError::Transport("net".into())), &mut b),
            PeerAction::BudgetState(BudgetState::Tolerate)
        );
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(
            decide_action(Err(ProbeError::Transport("net".into())), &mut b),
            PeerAction::BudgetState(BudgetState::Expired)
        );
    }
}
