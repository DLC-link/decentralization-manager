use std::time::Duration;

/// Minimum number of participants required for onboarding and kick workflows
pub const MIN_PARTICIPANTS: usize = 2;

/// Minimum number of participants required for contract workflows
/// Requires higher threshold for financial operations
pub const MIN_PARTICIPANTS_CONTRACTS: usize = 3;

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

/// Canton protocol version used for key export and topology operations
pub const CANTON_PROTOCOL_VERSION: i32 = 34;

/// Additional wait time in seconds for Canton topology propagation
/// After topology becomes effective, Canton needs time to propagate updates
/// to the sequencer's topology state. Without this wait, transactions may be
/// rejected with LOCAL_VERDICT_TIMEOUT.
pub const TOPOLOGY_PROPAGATION_DELAY_SECS: u64 = 30;

/// Cap on how long a peer tolerates BOTH Noise AND HTTP probe being
/// unreachable before bailing the run as Failed. Set above devnet's worst
/// observed coordinator restart (~120s for pod restart with image pull) but
/// short enough that a permanently-dead coordinator doesn't waste a full
/// integration run. See issue #173 fix design §7.
///
/// Overridable at runtime via the `DECPM_PROBE_BUDGET_SECS` env var — used
/// by the integration suite to shrink the budget to single-digit seconds
/// so a permanently-dead coordinator test doesn't add 3 minutes to every
/// run. Prod deployments should leave it unset (uses the default below).
pub const EXTENDED_TOLERANCE_BUDGET: Duration = Duration::from_secs(180);

/// Read the effective probe budget — environment override or the default
/// constant. Read once per `start_peer` invocation; not hot-path.
pub fn extended_tolerance_budget() -> Duration {
    match std::env::var("DECPM_PROBE_BUDGET_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        Some(secs) => Duration::from_secs(secs),
        None => EXTENDED_TOLERANCE_BUDGET,
    }
}

/// HTTP request timeout for the peer→coordinator cancel probe. Short because
/// the probe is on the failure path of an already-failing Noise call; we
/// don't want the probe to amplify latency.
pub const PROBE_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Max age of a probe timestamp the coordinator will accept, defending
/// against signed-probe replay. 30s tolerates modest clock skew (NTP-typical
/// under 1s); larger skew is an operator issue and surfaces as 403.
pub const PROBE_TIMESTAMP_TOLERANCE: Duration = Duration::from_secs(30);

// File name prefixes
/// Prefix for peer public key files
pub const PEER_KEYS_PREFIX: &str = "peer-public-keys";

/// Prefix for participant ID files
pub const PARTICIPANT_ID_PREFIX: &str = "participant-id";

/// Prefix for signed DNS proposal files
pub const SIGNED_DNS_PROPOSAL_PREFIX: &str = "signed-dns-proposal";

/// Prefix for signed P2P proposal files
pub const SIGNED_P2P_PROPOSALS_PREFIX: &str = "signed-p2p-proposals";

/// Prefix for submission signature files
pub const SUBMISSION_SIGNATURES_PREFIX: &str = "submission-signatures";

/// Prefix for signed kick proposal files
pub const SIGNED_KICK_PROPOSALS_PREFIX: &str = "signed-kick-proposals";

// File names
/// Namespace definition file name
pub const NAMESPACE_DEF_FILENAME: &str = "namespace_def.bin";

/// DNS proposal file name
pub const DNS_PROTO_FILENAME: &str = "dns_proto.bin";

/// P2P proposal file name
pub const P2P_PROTO_FILENAME: &str = "p2p_proto.bin";

/// DNS kick proposal file name
pub const DNS_KICK_PROTO_FILENAME: &str = "dns_kick_proto.bin";

/// P2P kick proposal file name
pub const P2P_KICK_PROTO_FILENAME: &str = "p2p_kick_proto.bin";

/// New namespace definition file name (for kick workflow)
pub const NEW_NAMESPACE_DEF_FILENAME: &str = "newNamespaceDef.bin";

/// Party ID file name
pub const PARTY_ID_FILENAME: &str = "partyId";

/// Kick target file name
pub const KICK_TARGET_FILENAME: &str = "kick-target";

/// Kick participant ID file name
pub const KICK_PARTICIPANT_ID_FILENAME: &str = "kick-participant-id";

/// New threshold file name
pub const NEW_THRESHOLD_FILENAME: &str = "new-threshold";

/// Prefix for prepared submission files
pub const PREPARED_SUBMISSION_PREFIX: &str = "prepared-submission";

// Base directory names (relative to root directory)
/// Data directory name (contains keys, workflow-data, and dars)
pub const DATA_DIR: &str = "data";

/// Noise private key filename (inside data/)
pub const NOISE_KEY_FILENAME: &str = "noise.key";

/// SQLite database filename (inside data/)
pub const DB_FILENAME: &str = "decpm.db";

/// DARs directory name (inside data/)
pub const DARS_DIR: &str = "dars";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_constants_have_expected_values() {
        assert_eq!(EXTENDED_TOLERANCE_BUDGET, Duration::from_secs(180));
        assert_eq!(PROBE_REQUEST_TIMEOUT, Duration::from_secs(2));
        assert_eq!(PROBE_TIMESTAMP_TOLERANCE, Duration::from_secs(30));
    }
}
