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
/// The peer event loop in `src/workflow/mod.rs` increments this counter on
/// any step-execution error and aborts the workflow once the threshold is
/// hit. 6 strikes × 2s sleep = 12s; introduced by PR #142 (raised from a
/// previously-hardcoded 3) because the localnet chaos suite's
/// cancel/decline/dismiss cleanup paths empirically need that headroom —
/// not just the Canton SignDns flake. Restoring the 3-strike value broke
/// the G10→G1 chaos boundary in CI (the post-cleanup workflow row
/// remained `InProgress` long enough for the next phase to collide with
/// it), so 6 stays.
///
/// The Canton-side `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE` transient
/// is handled separately at the `sign_transactions` call site with its
/// own env-configurable budget (`DPM_TOPOLOGY_RETRY_MAX_ATTEMPTS` ×
/// `DPM_TOPOLOGY_RETRY_DELAY_SECS`, defaults 30 × 2s = 60s). See
/// [`sign_transactions_with_topology_retry`](crate::workflow::onboarding::steps::proposals::sign::sign_transactions_with_topology_retry).
/// So this counter is now load-bearing for non-Canton transients only,
/// not for SignDns.
pub const MAX_CONSECUTIVE_STEP_FAILURES: usize = 6;

/// Canton protocol version used for key export and topology operations
pub const CANTON_PROTOCOL_VERSION: i32 = 34;

/// Additional wait time in seconds for Canton topology propagation
/// After topology becomes effective, Canton needs time to propagate updates
/// to the sequencer's topology state. Without this wait, transactions may be
/// rejected with LOCAL_VERDICT_TIMEOUT.
pub const TOPOLOGY_PROPAGATION_DELAY_SECS: u64 = 30;

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
