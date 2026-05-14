use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    consts::{DARS_DIR, DATA_DIR, DB_FILENAME, NOISE_KEY_FILENAME},
    participant_id::CantonId,
};

/// Network configuration - list of peers in the network
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct NetworkConfig {
    /// List of peers in the network
    pub peers: Vec<Peer>,
}

/// A peer in the network
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct Peer {
    /// Canton participant UID (e.g., "participant1::1220...")
    pub participant_id: CantonId,
    /// Human-readable name
    pub name: String,
    /// Network address (hostname or IP)
    pub address: String,
    /// Port for Noise protocol communication
    pub port: u16,
    /// Hex-encoded Noise static public key
    pub public_key: String,
    /// Canton party ID (optional, can be allocated dynamically if not provided)
    #[serde(default)]
    pub party: Option<String>,
}

impl NetworkConfig {
    /// Construct a NetworkConfig from a list of peers (e.g., loaded from DB)
    pub fn from_peers(peers: Vec<Peer>) -> Self {
        Self { peers }
    }

    /// Get the governance threshold for multi-sig operations
    /// Returns majority threshold: (n/2 + 1)
    pub fn governance_threshold(&self) -> u32 {
        ((self.peers.len() / 2) + 1) as u32
    }
}

/// Keycloak authentication configuration
///
/// Supports two authentication methods:
/// 1. Client credentials (M2M): Set `client_id` and `client_secret`
/// 2. Password flow: Set `client_id`, `username`, and `password`
#[derive(Clone, Debug, Default, Deserialize, Serialize, utoipa::ToSchema)]
pub struct KeycloakConfig {
    /// Keycloak server URL (e.g., "https://keycloak.example.com")
    pub url: String,
    /// Keycloak realm name
    pub realm: String,
    /// OAuth2 client ID
    pub client_id: String,
    /// Client secret for M2M (client_credentials) flow
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Username for password flow
    #[serde(default)]
    pub username: Option<String>,
    /// Password for password flow
    #[serde(default)]
    pub password: Option<String>,
}

/// Auth0 authentication configuration for frontend website gating.
///
/// Mutually exclusive with [`KeycloakConfig`] at the top level — each node
/// operator picks one or the other via environment variables at deploy time.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct Auth0Config {
    /// Auth0 tenant domain (e.g., "tenant.us.auth0.com")
    pub domain: String,
    /// Auth0 SPA client ID
    pub client_id: String,
    /// API audience identifier. Required for `getAccessTokenSilently()` to
    /// return a backend-validatable JWT rather than a userinfo-scoped token.
    #[serde(default)]
    pub audience: Option<String>,
}

/// Per-party Auth0 M2M credentials. Used to mint outbound access tokens the
/// backend sends to Canton when acting as the decentralized party.
///
/// Sibling of [`KeycloakConfig`] on [`PartyCredentials`]: when present, this
/// provider is used in place of Keycloak.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct Auth0M2MConfig {
    /// Auth0 tenant domain (e.g., "tenant.us.auth0.com")
    pub domain: String,
    /// Auth0 API audience (the API identifier the access token targets)
    pub audience: String,
    /// Auth0 M2M application client ID
    pub client_id: String,
    /// Auth0 M2M application client secret
    pub client_secret: String,
}

/// Package identifiers for Daml contracts (configurable per party)
#[derive(Clone, Debug, Default, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PackageConfig {
    pub governance_action: Option<String>,
    pub governance_core: Option<String>,
    pub governance_token_custody: Option<String>,
    pub governance_utility_credential: Option<String>,
    pub governance_utility_onboarding: Option<String>,
    pub utility_credential: Option<String>,
    pub utility_registry: Option<String>,
    pub vault: Option<String>,
    pub vault_governance: Option<String>,
}

/// Credentials for a specific decentralized party
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PartyCredentials {
    /// The decentralized party ID (shared among all members)
    pub dec_party_id: CantonId,
    /// The member party ID (local to this node, owns the credentials)
    pub member_party_id: CantonId,
    /// Canton/Ledger API user ID (must match JWT 'sub' claim, belongs to member_party)
    pub user_id: String,
    /// Keycloak authentication configuration. Empty/unused when `auth0` is set.
    #[serde(default)]
    pub keycloak: KeycloakConfig,
    /// Auth0 M2M authentication. When `Some`, used in preference to `keycloak`.
    #[serde(default)]
    pub auth0: Option<Auth0M2MConfig>,
    /// Package identifiers for deployed Daml contracts
    #[serde(default)]
    pub packages: PackageConfig,
}

/// Timeout configuration
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Timeouts {
    pub handshake_timeout_secs: u64,
    pub message_timeout_secs: u64,
    pub connection_retry_attempts: u32,
    pub connection_retry_delay_secs: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            handshake_timeout_secs: 30,
            message_timeout_secs: 120,
            connection_retry_attempts: 3,
            connection_retry_delay_secs: 5,
        }
    }
}

/// Configuration for the bounded retry wrapper around peer Noise calls
/// (`send_noise_message_with_retry`). Defaults match the spec working
/// hypothesis: 5s × 2 attempts, 250ms backoff between attempts.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct NoiseRetryConfig {
    /// Per-attempt timeout in seconds (applied independently to TCP connect
    /// and to the Noise/HTTP request budget).
    pub per_attempt_timeout_secs: u64,
    /// Total attempts (initial + retries). 2 means "1 retry."
    pub max_attempts: usize,
    /// Fixed backoff between attempts in milliseconds.
    pub backoff_ms: u64,
}

impl Default for NoiseRetryConfig {
    fn default() -> Self {
        Self {
            per_attempt_timeout_secs: 5,
            max_attempts: 2,
            backoff_ms: 250,
        }
    }
}

impl NoiseRetryConfig {
    pub fn per_attempt_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.per_attempt_timeout_secs)
    }

    pub fn backoff(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.backoff_ms)
    }
}

/// Individual node configuration
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct NodeConfig {
    pub node: NodeInfo,
    pub canton: CantonConfig,
    pub timeouts: Timeouts,
    pub noise_retry: NoiseRetryConfig,
    /// Top-level Keycloak config for frontend website gating
    pub keycloak: Option<KeycloakConfig>,
    /// Top-level Auth0 config for frontend website gating (mutually exclusive
    /// with `keycloak` — operator picks one via env vars at deploy time).
    pub auth0: Option<Auth0Config>,
    /// Root directory containing data/ subdirectory
    #[serde(skip)]
    root_dir: PathBuf,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            node: NodeInfo::default(),
            canton: CantonConfig::default(),
            timeouts: Timeouts::default(),
            noise_retry: NoiseRetryConfig::default(),
            keycloak: None,
            auth0: None,
            root_dir: PathBuf::new(),
        }
    }
}

/// Node-specific information
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct NodeInfo {
    /// Canton participant ID for this node (e.g., "participant1::1220...")
    pub participant_id: Option<CantonId>,
    /// Address to listen on for Noise protocol connections
    pub listen_address: String,
    /// Port to listen on for Noise protocol connections
    pub port: u16,
    /// Public address that other peers should use to connect to this node
    pub public_address: Option<String>,
}

impl NodeInfo {
    /// Get the public address for this node (for sharing with peers).
    /// Falls back to listen_address if public_address is not set.
    pub fn public_address(&self) -> &str {
        self.public_address
            .as_deref()
            .unwrap_or(&self.listen_address)
    }
}

impl Default for NodeInfo {
    fn default() -> Self {
        Self {
            participant_id: None,
            listen_address: "0.0.0.0".to_string(),
            port: 9000,
            public_address: None,
        }
    }
}

/// Default Keycloak configuration values for a network
pub struct KeycloakDefaults {
    /// Keycloak server URL
    pub url: String,
    /// Keycloak realm name
    pub realm: String,
}

/// Canton Network environment
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Devnet,
    Testnet,
    Mainnet,
}

impl Network {
    /// Get the DSO API base URL for this network
    pub fn dso_url(&self) -> &str {
        match self {
            Network::Devnet => "https://docs.dev.global.canton.network.sync.global/dso",
            Network::Testnet => "https://docs.test.global.canton.network.sync.global/dso",
            Network::Mainnet => "https://docs.global.canton.network.sync.global/dso",
        }
    }

    /// Get the DA Utility operator API URL for this network
    pub fn operator_url(&self) -> &str {
        match self {
            Network::Devnet => {
                "https://api.utilities.digitalasset-dev.com/api/utilities/v0/operator"
            }
            Network::Testnet => {
                "https://api.utilities.digitalasset-staging.com/api/utilities/v0/operator"
            }
            Network::Mainnet => "https://api.utilities.digitalasset.com/api/utilities/v0/operator",
        }
    }

    /// Get default Keycloak configuration for this network
    pub fn keycloak_defaults(&self) -> KeycloakDefaults {
        match self {
            Network::Devnet => KeycloakDefaults {
                url: "https://keycloak.dev.canton.ibtc.network".to_string(),
                realm: "ibtc-catalyst-devnet".to_string(),
            },
            Network::Testnet => KeycloakDefaults {
                url: String::new(),
                realm: String::new(),
            },
            Network::Mainnet => KeycloakDefaults {
                url: String::new(),
                realm: String::new(),
            },
        }
    }
}

/// Default package identifiers used for new party configurations
pub fn default_package_config() -> PackageConfig {
    PackageConfig {
        governance_action: Some("#governance-action-v0".to_string()),
        governance_core: Some("#governance-core-v0".to_string()),
        governance_token_custody: Some("#governance-token-custody-v0".to_string()),
        governance_utility_credential: Some("#governance-utility-credential-v0".to_string()),
        governance_utility_onboarding: Some("#governance-utility-onboarding-v0".to_string()),
        utility_credential: Some("#utility-credential-app-v0".to_string()),
        utility_registry: Some("#utility-registry-app-v0".to_string()),
        vault: Some("#bitsafe-vault-v0-rc8".to_string()),
        vault_governance: Some("#bitsafe-vault-governance-v0-rc8".to_string()),
    }
}

/// Canton participant configuration
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct CantonConfig {
    pub admin_api_host: String,
    pub admin_api_port: u16,
    pub ledger_api_host: String,
    pub ledger_api_port: u16,
    pub synchronizer: String,
    /// Canton Network environment (devnet, testnet, mainnet)
    pub network: Network,
}

impl Default for CantonConfig {
    fn default() -> Self {
        Self {
            admin_api_host: "127.0.0.1".to_string(),
            admin_api_port: 5002,
            ledger_api_host: "127.0.0.1".to_string(),
            ledger_api_port: 5001,
            synchronizer: "global".to_string(),
            network: Network::Devnet,
        }
    }
}

impl NodeConfig {
    /// Create a NodeConfig with the given root directory
    pub fn with_root_dir<P: AsRef<Path>>(mut self, root_dir: P) -> Self {
        self.root_dir = root_dir.as_ref().to_path_buf();
        self
    }

    /// Get the full Admin API URL
    pub fn admin_api_url(&self) -> String {
        format!(
            "http://{}:{}",
            self.canton.admin_api_host, self.canton.admin_api_port
        )
    }

    /// Get the full Ledger API URL
    pub fn ledger_api_url(&self) -> String {
        format!(
            "http://{}:{}",
            self.canton.ledger_api_host, self.canton.ledger_api_port
        )
    }

    /// Get the synchronizer name
    pub fn synchronizer(&self) -> &str {
        &self.canton.synchronizer
    }

    /// Get the data directory
    pub fn data_dir(&self) -> PathBuf {
        self.root_dir.join(DATA_DIR)
    }

    /// Get the path to the noise key file
    pub fn key_file_path(&self) -> PathBuf {
        self.data_dir().join(NOISE_KEY_FILENAME)
    }

    /// Get the dars directory
    pub fn dars_dir(&self) -> PathBuf {
        self.data_dir().join(DARS_DIR)
    }

    /// Get the path to the SQLite database file
    pub fn db_path(&self) -> PathBuf {
        self.data_dir().join(DB_FILENAME)
    }

    /// Get the root directory
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Get the participant ID, panicking if not resolved
    ///
    /// Call `resolve_participant_id` before using this method.
    pub fn participant_id(&self) -> &CantonId {
        self.node
            .participant_id
            .as_ref()
            .expect("participant_id not resolved - call resolve_participant_id first")
    }

    /// Check if participant_id is already set
    pub fn has_participant_id(&self) -> bool {
        self.node.participant_id.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer(index: u8, pub_key: &str) -> Peer {
        let namespace = format!("1220{:0>64}", format!("{index:02x}"));
        Peer {
            participant_id: CantonId::parse(&format!("node{index}::{namespace}")).unwrap(),
            name: format!("Node {index}"),
            address: format!("10.0.1.{index}"),
            port: 9000,
            public_key: pub_key.to_string(),
            party: None,
        }
    }

    #[test]
    fn test_governance_threshold() {
        let network = NetworkConfig {
            peers: vec![
                test_peer(1, "abc123"),
                test_peer(2, "def456"),
                test_peer(3, "ghi789"),
            ],
        };

        assert_eq!(network.governance_threshold(), 2);
    }

    #[test]
    fn test_keycloak_defaults_devnet() {
        let defaults = Network::Devnet.keycloak_defaults();
        assert_eq!(defaults.url, "https://keycloak.dev.canton.ibtc.network");
        assert_eq!(defaults.realm, "ibtc-catalyst-devnet");
    }

    #[test]
    fn test_keycloak_defaults_testnet_mainnet_empty() {
        for network in [Network::Testnet, Network::Mainnet] {
            let defaults = network.keycloak_defaults();
            assert!(defaults.url.is_empty());
            assert!(defaults.realm.is_empty());
        }
    }

    #[test]
    fn test_default_package_config() {
        let packages = default_package_config();
        assert_eq!(
            packages.governance_action.as_deref(),
            Some("#governance-action-v0"),
        );
        assert_eq!(
            packages.governance_core.as_deref(),
            Some("#governance-core-v0"),
        );
        assert_eq!(
            packages.governance_token_custody.as_deref(),
            Some("#governance-token-custody-v0"),
        );
        assert_eq!(
            packages.governance_utility_credential.as_deref(),
            Some("#governance-utility-credential-v0"),
        );
        assert_eq!(
            packages.governance_utility_onboarding.as_deref(),
            Some("#governance-utility-onboarding-v0"),
        );
        assert_eq!(
            packages.utility_credential.as_deref(),
            Some("#utility-credential-app-v0"),
        );
        assert_eq!(
            packages.utility_registry.as_deref(),
            Some("#utility-registry-app-v0"),
        );
        assert_eq!(packages.vault.as_deref(), Some("#bitsafe-vault-v0-rc8"));
        assert_eq!(
            packages.vault_governance.as_deref(),
            Some("#bitsafe-vault-governance-v0-rc8"),
        );
    }
}
