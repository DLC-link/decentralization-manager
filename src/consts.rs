/// Minimum number of participants required for the workflow
pub const MIN_PARTICIPANTS: usize = 3;

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

// File names
/// Namespace definition file name
pub const NAMESPACE_DEF_FILENAME: &str = "namespace_def.bin";

/// DNS proposal file name
pub const DNS_PROTO_FILENAME: &str = "dns_proto.bin";

/// P2P proposal file name
pub const P2P_PROTO_FILENAME: &str = "p2p_proto.bin";

/// Prefix for prepared submission files
pub const PREPARED_SUBMISSION_PREFIX: &str = "prepared-submission";

// Directory names
/// Ledger submissions directory name
pub const LEDGER_SUBMISSIONS_DIR: &str = "ledger-submissions";

/// Prepared submissions subdirectory name
pub const PREPARED_DIR: &str = "prepared";

/// Execution directory name
pub const EXECUTION_DIR: &str = "execution";

/// Signatures subdirectory name
pub const SIGNATURES_DIR: &str = "signatures";

// Hard-coded stuff
/// Party ID prefix used for constructing decentralized party identifiers
/// Format: "{PARTY_ID_PREFIX}::<namespace>"
pub const PARTY_ID_PREFIX: &str = "cbtc-network";

/// Name prefix for namespace signing keys
pub const NAMESPACE_KEY_NAME: &str = "cbtc-network-namespace";

/// Name prefix for DAML transaction signing keys
pub const DAML_KEY_NAME: &str = "cbtc-network-daml-transactions";

/// Ledger API user ID for submission operations
pub const LEDGER_API_USER_ID: &str = "ledger-api-user";

/// Package ID for CBTC governance contracts
pub const CBTC_GOVERNANCE_PACKAGE_ID: &str = "#cbtc-governance";

/// Package ID for CBTC deposit and withdraw contracts
pub const CBTC_PACKAGE_ID: &str = "#cbtc";

/// DAML module name for governance contracts
pub const CBTC_GOVERNANCE_MODULE: &str = "CBTC.Governance";

/// DAML module name for deposit account contracts
pub const CBTC_DEPOSIT_ACCOUNT_MODULE: &str = "CBTC.DepositAccount";

/// DAML module name for withdraw account contracts
pub const CBTC_WITHDRAW_ACCOUNT_MODULE: &str = "CBTC.WithdrawAccount";

/// Entity name for governance rules template
pub const CBTC_GOVERNANCE_RULES_ENTITY: &str = "CBTCGovernanceRules";

/// Entity name for deposit account rules template
pub const CBTC_DEPOSIT_ACCOUNT_RULES_ENTITY: &str = "CBTCDepositAccountRules";

/// Entity name for withdraw account rules template
pub const CBTC_WITHDRAW_ACCOUNT_RULES_ENTITY: &str = "CBTCWithdrawAccountRules";

/// Command ID for creating governance rules contract
pub const CREATE_GOVERNANCE_RULES_COMMAND_ID: &str = "create-govR";

/// Command ID for creating deposit account rules contract
pub const CREATE_DEPOSIT_ACCOUNT_RULES_COMMAND_ID: &str = "create-daR";

/// Command ID for creating withdraw account rules contract
pub const CREATE_WITHDRAW_ACCOUNT_RULES_COMMAND_ID: &str = "create-waR";

/// Instrument ID for CBTC
pub const CBTC_INSTRUMENT_ID: &str = "CBTC";

/// Party hint for operator party allocation
pub const OPERATOR_PARTY_HINT: &str = "operator";
