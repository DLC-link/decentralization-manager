/// Maximum number of retry attempts for topology propagation checks.
/// Default value; the actual budget is read via [`topology_retry_max_attempts`].
pub const TOPOLOGY_RETRY_MAX_ATTEMPTS: usize = 30;

/// Delay in seconds between retry attempts for topology operations.
/// Default value; the actual delay is read via [`topology_retry_delay_secs`].
pub const TOPOLOGY_RETRY_DELAY_SECS: u64 = 2;

/// Maximum retry attempts for topology propagation, configurable at runtime
/// via the `DECPM_TOPOLOGY_RETRY_MAX_ATTEMPTS` env var. Defaults to
/// [`TOPOLOGY_RETRY_MAX_ATTEMPTS`] (30) when unset or unparseable.
///
/// On devnet, Canton's topology read API response time varies significantly
/// across runs — a 60s budget (30 × 2s) sometimes covers the worst case,
/// sometimes doesn't. Operators running against a slow synchronizer can
/// raise this without recompiling.
pub fn topology_retry_max_attempts() -> usize {
    std::env::var("DECPM_TOPOLOGY_RETRY_MAX_ATTEMPTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(TOPOLOGY_RETRY_MAX_ATTEMPTS)
}

/// Delay between topology-poll attempts, configurable via the
/// `DECPM_TOPOLOGY_RETRY_DELAY_SECS` env var. Defaults to
/// [`TOPOLOGY_RETRY_DELAY_SECS`] (2) when unset or unparseable.
pub fn topology_retry_delay_secs() -> u64 {
    std::env::var("DECPM_TOPOLOGY_RETRY_DELAY_SECS")
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
/// Minimum dec-party-manager version every workflow participant must run.
///
/// 0.1.9 introduced concurrent multi-instance workflows with a breaking Noise
/// wire format (version byte + instance routing); older builds cannot even
/// parse the new frames. Workflow starts probe every invitee's `Health` and
/// refuse to send invites unless each one POSITIVELY reports a version >= this
/// — an old build answers the (unparseable-to-it) probe with a 503, so
/// "no verifiable version" is treated as incompatible rather than assumed OK.
pub const MIN_PEER_VERSION: &str = "0.1.9";

/// Retry budget for the peer's decline notification to the coordinator.
///
/// Deliberately NOT the fast-transport `noise_retry` profile (2 attempts,
/// 250 ms backoff): a coordinator run only becomes routable once its spawned
/// task has sent invites, slept its peer-grace period, and called
/// `set_active` — ~2s after start. A peer declining the moment the invite
/// card appears (a real operator pattern, reproduced by G14 in CI) hits that
/// window and gets 503s; the fast profile's retries are exhausted inside it
/// and the coordinator's human-paced run then hangs forever. 5 attempts at
/// 2s spacing rides out any plausible init window.
pub const DECLINE_NOTIFY_MAX_ATTEMPTS: usize = 5;
pub const DECLINE_NOTIFY_BACKOFF_SECS: u64 = 2;

pub const MAX_CONSECUTIVE_NO_WORKFLOW_POLLS: usize = 4;

/// Delay before a peer re-polls the coordinator after a `Wait` reply, in
/// milliseconds.
/// Default value; the actual delay is read via [`peer_wait_poll_delay_ms`].
pub const PEER_WAIT_POLL_DELAY_MS: u64 = 2000;

/// How long a peer waits before asking the coordinator for its next command
/// again, configurable via the `DECPM_PEER_WAIT_POLL_DELAY_MS` env var.
/// Defaults to [`PEER_WAIT_POLL_DELAY_MS`] (2000) when unset or unparseable.
///
/// This is the cadence of the peer event loop in `workflow/mod.rs`: whenever
/// the coordinator has no command ready it answers `Wait`, and the peer sleeps
/// this long before asking again. It therefore quantizes every multi-step
/// workflow — a run that needs N polls cannot finish faster than N times this
/// value, whatever the work actually costs.
///
/// 2s is right against a real synchronizer, where each coordinator step is a
/// 10-30s Canton round trip and the poll is noise. On a single-container
/// localnet the step lands in milliseconds, so the quantization *is* the
/// runtime; the integration-test harness lowers it.
pub fn peer_wait_poll_delay_ms() -> u64 {
    std::env::var("DECPM_PEER_WAIT_POLL_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(PEER_WAIT_POLL_DELAY_MS)
}

/// Canton protocol version used for key export and topology operations.
/// Bumped 34 -> 35 alongside the localnet 0.6.7 -> 0.6.11 test target; the
/// network (testnet) has live-upgraded to protocol version 35.
pub const CANTON_PROTOCOL_VERSION: i32 = 35;

/// Additional wait time in seconds for Canton topology propagation
/// After topology becomes effective, Canton needs time to propagate updates
/// to the sequencer's topology state. Without this wait, transactions may be
/// rejected with LOCAL_VERDICT_TIMEOUT.
///
/// Default value; the actual delay is read via
/// [`topology_propagation_delay_secs`].
pub const TOPOLOGY_PROPAGATION_DELAY_SECS: u64 = 30;

/// Post-submission topology propagation wait, configurable via the
/// `DECPM_TOPOLOGY_PROPAGATION_DELAY_SECS` env var. Defaults to
/// [`TOPOLOGY_PROPAGATION_DELAY_SECS`] (30) when unset or unparseable.
///
/// The 30s default is sized for a real multi-node synchronizer, where the
/// sequencer's topology state settles well after the transaction becomes
/// effective. A single-container localnet settles in under a second, so the
/// integration-test harness lowers it.
pub fn topology_propagation_delay_secs() -> u64 {
    std::env::var("DECPM_TOPOLOGY_PROPAGATION_DELAY_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(TOPOLOGY_PROPAGATION_DELAY_SECS)
}

// ---------------------------------------------------------------------------
// On-ledger coordination (design sections 3, 9)
// ---------------------------------------------------------------------------

/// The `#package-name` ref of the coordination package. Node-level, not per
/// party: the registry entry exists before any decentralized party does.
/// Default value; the actual ref is read via [`coordination_package_ref`].
pub const COORDINATION_PACKAGE_REF: &str = "#decman-coordination-v1";

/// The coordination package ref, overridable with
/// `DECPM_COORDINATION_PACKAGE_REF`. Defaults to [`COORDINATION_PACKAGE_REF`].
pub fn coordination_package_ref() -> String {
    std::env::var("DECPM_COORDINATION_PACKAGE_REF")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| COORDINATION_PACKAGE_REF.to_string())
}

/// The coordination protocol version this build speaks. A proposer refuses to
/// start a workflow with an invitee whose registry entry reports a lower
/// `coordinationVersion` (design D3).
pub const COORDINATION_VERSION: i64 = 1;

/// Default seconds between `DecmanNode_Heartbeat` exercises.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 3600;

/// Heartbeat cadence, configurable via `DECPM_HEARTBEAT_INTERVAL_SECS`.
/// Zero is clamped to one second so the heartbeat can never spin.
pub fn heartbeat_interval_secs() -> u64 {
    std::env::var("DECPM_HEARTBEAT_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(HEARTBEAT_INTERVAL_SECS)
        .max(1)
}

/// Default floor the `DecmanNode` template enforces between heartbeats.
pub const HEARTBEAT_MIN_INTERVAL_SECS: u64 = 60;

/// The heartbeat floor written into `DecmanNode.minHeartbeatIntervalSecs`,
/// configurable via `DECPM_HEARTBEAT_MIN_INTERVAL_SECS`. Clamped to
/// `[1, heartbeat_interval_secs()]` because the template's `ensure` rejects a
/// floor above the interval.
pub fn heartbeat_min_interval_secs() -> u64 {
    std::env::var("DECPM_HEARTBEAT_MIN_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(HEARTBEAT_MIN_INTERVAL_SECS)
        .clamp(1, heartbeat_interval_secs())
}

/// Default multiplier of a peer's heartbeat interval after which its entry
/// reads as `Stale`.
pub const PEER_STALE_FACTOR: u64 = 3;

/// Staleness multiplier, configurable via `DECPM_PEER_STALE_FACTOR`. Zero is
/// clamped to one so a fresh heartbeat is never stale.
pub fn peer_stale_factor() -> u64 {
    std::env::var("DECPM_PEER_STALE_FACTOR")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(PEER_STALE_FACTOR)
        .max(1)
}

/// Default seconds between observer ticks.
pub const OBSERVER_POLL_SECS: u64 = 3;

/// Observer tick cadence, configurable via `DECPM_OBSERVER_POLL_SECS`.
/// Mainnet guidance is 10. Zero is clamped to one.
pub fn observer_poll_secs() -> u64 {
    std::env::var("DECPM_OBSERVER_POLL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(OBSERVER_POLL_SECS)
        .max(1)
}

/// Default lifetime of a `WorkflowProposal`: seven days.
pub const PROPOSAL_TTL_SECS: u64 = 604_800;

/// Proposal lifetime, configurable via `DECPM_PROPOSAL_TTL_SECS`. Zero is
/// clamped to one because the template requires `expiresAt > createdAt`.
pub fn proposal_ttl_secs() -> u64 {
    std::env::var("DECPM_PROPOSAL_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(PROPOSAL_TTL_SECS)
        .max(1)
}

/// Seconds between the observer's unfiltered proposal scans and archive
/// sweeps (design D5). The scan feeds the UI only and never signs.
pub const UNSOLICITED_SCAN_INTERVAL_SECS: u64 = 60;

/// Default for `DECPM_AUTO_UPLOAD_COORDINATION_DAR` (design D8).
pub const AUTO_UPLOAD_COORDINATION_DAR: bool = true;

/// Whether a startup task uploads and vets the embedded coordination DAR,
/// configurable via `DECPM_AUTO_UPLOAD_COORDINATION_DAR` (`true`/`false`,
/// `1`/`0`). Defaults to [`AUTO_UPLOAD_COORDINATION_DAR`].
pub fn auto_upload_coordination_dar() -> bool {
    match std::env::var("DECPM_AUTO_UPLOAD_COORDINATION_DAR") {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "no" | "off" => false,
            "1" | "true" | "yes" | "on" => true,
            _ => AUTO_UPLOAD_COORDINATION_DAR,
        },
        Err(_) => AUTO_UPLOAD_COORDINATION_DAR,
    }
}

/// Directory name of the ACS spool inside the data directory (design D9).
pub const ACS_SPOOL_DIR_NAME: &str = "acs";

/// Where add-party ACS snapshots are spooled, configurable via
/// `DECPM_ACS_SPOOL_DIR`. Defaults to `{data_dir}/acs`.
pub fn acs_spool_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    match std::env::var("DECPM_ACS_SPOOL_DIR") {
        Ok(v) if !v.trim().is_empty() => std::path::PathBuf::from(v),
        _ => data_dir.join(ACS_SPOOL_DIR_NAME),
    }
}

// Base directory names (relative to root directory)
/// Data directory name (contains the Noise key, SQLite database, and DARs)
pub const DATA_DIR: &str = "data";

/// Noise private key filename (inside data/)
pub const NOISE_KEY_FILENAME: &str = "noise.key";

/// SQLite database filename (inside data/)
pub const DB_FILENAME: &str = "decpm.db";

/// DARs directory name (inside data/)
pub const DARS_DIR: &str = "dars";
