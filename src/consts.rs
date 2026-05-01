/// Minimum number of participants required for onboarding and kick workflows
pub const MIN_PARTICIPANTS: usize = 2;

/// Minimum number of participants required for contract workflows
/// Requires higher threshold for financial operations
pub const MIN_PARTICIPANTS_CONTRACTS: usize = 3;

/// Maximum number of retry attempts for topology propagation checks
pub const TOPOLOGY_RETRY_MAX_ATTEMPTS: usize = 30;

/// Delay in seconds between retry attempts for topology operations
pub const TOPOLOGY_RETRY_DELAY_SECS: u64 = 2;

/// Canton protocol version used for key export and topology operations
pub const CANTON_PROTOCOL_VERSION: i32 = 34;

/// Additional wait time in seconds for Canton topology propagation
/// After topology becomes effective, Canton needs time to propagate updates
/// to the sequencer's topology state. Without this wait, transactions may be
/// rejected with LOCAL_VERDICT_TIMEOUT.
pub const TOPOLOGY_PROPAGATION_DELAY_SECS: u64 = 30;

// File name prefixes
/// Prefix for attestor public key files
pub const ATTESTOR_KEYS_PREFIX: &str = "attestor-public-keys";

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
