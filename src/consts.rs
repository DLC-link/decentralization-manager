/// Maximum number of retry attempts for topology propagation checks.
/// Default value; the actual budget is read via [`topology_retry_max_attempts`].
pub const TOPOLOGY_RETRY_MAX_ATTEMPTS: usize = 30;

/// Delay in seconds between retry attempts for topology operations.
/// Default value; the actual delay is read via [`topology_retry_delay_secs`].
pub const TOPOLOGY_RETRY_DELAY_SECS: u64 = 2;

/// Maximum retry attempts for topology propagation, configurable at runtime
/// via the `DPM_TOPOLOGY_RETRY_MAX_ATTEMPTS` env var. Defaults to
/// [`TOPOLOGY_RETRY_MAX_ATTEMPTS`] (30) when unset or unparseable.
///
/// On devnet, Canton's topology read API response time varies significantly
/// across runs — a 60s budget (30 × 2s) sometimes covers the worst case,
/// sometimes doesn't. Operators running against a slow synchronizer can
/// raise this without recompiling.
pub fn topology_retry_max_attempts() -> usize {
    std::env::var("DPM_TOPOLOGY_RETRY_MAX_ATTEMPTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(TOPOLOGY_RETRY_MAX_ATTEMPTS)
}

/// Delay between topology-poll attempts, configurable via the
/// `DPM_TOPOLOGY_RETRY_DELAY_SECS` env var. Defaults to
/// [`TOPOLOGY_RETRY_DELAY_SECS`] (2) when unset or unparseable.
pub fn topology_retry_delay_secs() -> u64 {
    std::env::var("DPM_TOPOLOGY_RETRY_DELAY_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(TOPOLOGY_RETRY_DELAY_SECS)
}

/// Maximum number of consecutive failures before a peer-side workflow step
/// aborts the whole workflow.
///
/// The peer event loop in `src/workflow/mod.rs` retries each step on
/// Canton-side errors (the most common being
/// `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE` when the synchronizer
/// hasn't fully reconciled a freshly-restarted participant's signing keys
/// yet). The previous hardcoded 3-attempt × 2s budget (6s window) was
/// tuned for localnet's docker-compose Canton, which reconciles in ms.
/// On devnet's kubectl-tunneled Canton — especially right after a chaos
/// phase restart — the reconciliation can take 10–20s; all three attempts
/// can land inside the same slow window and the peer aborts unnecessarily.
///
/// 6 attempts × 2s = 12s gives the same retry cadence with twice the
/// total window, which has comfortably covered the observed Canton
/// reconciliation lag in lived devnet runs. Production behavior on a
/// healthy synchronizer is unchanged because the first attempt continues
/// to succeed the vast majority of the time.
pub const MAX_CONSECUTIVE_STEP_FAILURES: usize = 6;

/// How many consecutive "no active workflow" replies (HTTP 503 from the
/// coordinator's always-on listener) a peer tolerates before abandoning a
/// resumed run.
///
/// A peer reaches the listener but finds no workflow registered in two cases:
///   1. Transient — the coordinator restarted and `recover_in_progress_workflows`
///      hasn't re-registered the active-workflow slot yet (a sub-second-to-a-few-
///      seconds window after the listener starts accepting).
///   2. Permanent — the coordinator's workflow was cancelled or dismissed while
///      this peer was offline, so the slot will never be populated.
///
/// Replying `Wait` to case 2 would leave the peer polling forever, keeping its
/// run InProgress and the node perpetually "busy" to invite / pre-flight checks.
/// We instead give up after this many polls. The counter resets on any real
/// reply, so case 1 rides through. 4 polls × 5s ≈ 20s: long enough to cover a
/// slow resume, short enough that a dismissed run is cleaned up promptly.
pub const MAX_CONSECUTIVE_NO_WORKFLOW_POLLS: usize = 4;

/// Canton protocol version used for key export and topology operations
pub const CANTON_PROTOCOL_VERSION: i32 = 34;

/// Additional wait time in seconds for Canton topology propagation
/// After topology becomes effective, Canton needs time to propagate updates
/// to the sequencer's topology state. Without this wait, transactions may be
/// rejected with LOCAL_VERDICT_TIMEOUT.
pub const TOPOLOGY_PROPAGATION_DELAY_SECS: u64 = 30;

// Base directory names (relative to root directory)
/// Data directory name (contains the Noise key, SQLite database, and DARs)
pub const DATA_DIR: &str = "data";

/// Noise private key filename (inside data/)
pub const NOISE_KEY_FILENAME: &str = "noise.key";

/// SQLite database filename (inside data/)
pub const DB_FILENAME: &str = "decpm.db";

/// DARs directory name (inside data/)
pub const DARS_DIR: &str = "dars";
