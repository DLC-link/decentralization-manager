use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::{
    consts::{
        CONFIG_DIR, DARS_DIR, DATA_DIR, DB_FILENAME, NODE_CONFIG_FILENAME, NOISE_KEY_FILENAME,
        WORKFLOW_DATA_DIR,
    },
    error::Result,
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

    /// Load network configuration from a CSV file
    ///
    /// CSV format: participant_id,name,address,port,public_key,party
    /// Creates an empty file with header if it doesn't exist
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // Create file with header if it doesn't exist
        if !path.exists() {
            tracing::info!("Creating empty peers.csv at '{}'", path.display());
            tokio::fs::write(path, "participant_id,name,address,port,public_key,party\n")
                .await
                .with_context(|| format!("Failed to create peers config '{}'", path.display()))?;
        }

        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read peers config '{}'", path.display()))?;

        let mut reader = csv::Reader::from_reader(content.as_bytes());
        let mut peers = Vec::new();

        for result in reader.deserialize() {
            let peer: Peer = result.with_context(|| {
                format!("Failed to parse peer in '{path}'", path = path.display())
            })?;
            peers.push(peer);
        }

        Ok(NetworkConfig { peers })
    }

    /// Save network configuration to a CSV file
    pub async fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result {
        let path = path.as_ref();
        let mut writer = csv::Writer::from_writer(Vec::new());

        for peer in &self.peers {
            writer
                .serialize(peer)
                .with_context(|| "Failed to serialize peer")?;
        }

        let data = writer
            .into_inner()
            .with_context(|| "Failed to finalize CSV")?;

        tokio::fs::write(path, data)
            .await
            .with_context(|| format!("Failed to write peers config '{}'", path.display()))?;

        Ok(())
    }

    /// Get the governance threshold for multi-sig operations
    /// Returns majority threshold: (n/2 + 1)
    pub fn governance_threshold(&self) -> u32 {
        ((self.peers.len() / 2) + 1) as u32
    }

    /// Get peer by participant ID string
    pub fn get_peer(&self, id: &str) -> Option<&Peer> {
        self.peers
            .iter()
            .find(|p| p.participant_id.to_string() == id)
    }

    /// Create a map of public keys to peer IDs for verification
    pub fn get_public_key_allowlist(&self) -> HashMap<String, String> {
        self.peers
            .iter()
            .map(|p| (p.public_key.clone(), p.participant_id.to_string()))
            .collect()
    }
}

/// Keycloak authentication configuration
///
/// Supports two authentication methods:
/// 1. Client credentials (M2M): Set `client_id` and `client_secret`
/// 2. Password flow: Set `client_id`, `username`, and `password`
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
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

/// Package identifiers for Daml contracts (configurable per party)
#[derive(Clone, Debug, Default, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PackageConfig {
    pub governance_core: Option<String>,
    pub governance_token_custody: Option<String>,
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
    /// Keycloak authentication configuration
    pub keycloak: KeycloakConfig,
    /// Package identifiers for deployed Daml contracts
    #[serde(default)]
    pub packages: PackageConfig,
}

/// Timeout configuration
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct Timeouts {
    #[serde(default = "default_handshake_timeout")]
    pub handshake_timeout_secs: u64,
    #[serde(default = "default_message_timeout")]
    pub message_timeout_secs: u64,
    #[serde(default = "default_connection_retry_attempts")]
    pub connection_retry_attempts: u32,
    #[serde(default = "default_connection_retry_delay")]
    pub connection_retry_delay_secs: u64,
}

fn default_handshake_timeout() -> u64 {
    30
}
fn default_message_timeout() -> u64 {
    120
}
fn default_connection_retry_attempts() -> u32 {
    3
}
fn default_connection_retry_delay() -> u64 {
    5
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            handshake_timeout_secs: default_handshake_timeout(),
            message_timeout_secs: default_message_timeout(),
            connection_retry_attempts: default_connection_retry_attempts(),
            connection_retry_delay_secs: default_connection_retry_delay(),
        }
    }
}

/// Individual node configuration
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct NodeConfig {
    pub node: NodeInfo,
    pub canton: CantonConfig,
    #[serde(default)]
    pub timeouts: Timeouts,
    /// Top-level Keycloak config for frontend website gating
    #[serde(default)]
    pub keycloak: Option<KeycloakConfig>,
    /// Per-party credentials with Keycloak auth (multiple parties supported)
    #[serde(default)]
    pub parties: Vec<PartyCredentials>,
    /// Root directory containing config/ and data/ subdirectories
    #[serde(skip)]
    root_dir: PathBuf,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            node: NodeInfo::default(),
            canton: CantonConfig::default(),
            timeouts: Timeouts::default(),
            keycloak: None,
            parties: Vec::new(),
            root_dir: PathBuf::new(),
        }
    }
}

/// Node-specific information
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct NodeInfo {
    /// Canton participant ID for this node (e.g., "participant1::1220...")
    /// If not specified, it will be queried from Canton and saved to the config.
    #[serde(default)]
    pub participant_id: Option<CantonId>,
    /// Address to listen on for Noise protocol connections (use 0.0.0.0 to listen on all interfaces)
    #[serde(default = "default_listen_address")]
    pub listen_address: String,
    /// Port to listen on for Noise protocol connections
    #[serde(default = "default_noise_port")]
    pub port: u16,
    /// Public address that other peers should use to connect to this node.
    /// This is the address shared when exporting peer info.
    #[serde(default)]
    pub public_address: Option<String>,
}

fn default_listen_address() -> String {
    "0.0.0.0".to_string()
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

fn default_noise_port() -> u16 {
    9000
}

impl Default for NodeInfo {
    fn default() -> Self {
        Self {
            participant_id: None,
            listen_address: default_listen_address(),
            port: default_noise_port(),
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
        governance_core: Some("#governance-core-v0-rc1".to_string()),
        governance_token_custody: Some("#governance-token-custody-v0-rc1".to_string()),
        utility_credential: Some("#utility-credential-app-v0".to_string()),
        utility_registry: Some("#utility-registry-app-v0".to_string()),
        vault: Some("#bitsafe-vault-v0-rc8".to_string()),
        vault_governance: Some("#bitsafe-vault-governance-v0-rc8".to_string()),
    }
}

/// Canton participant configuration
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct CantonConfig {
    pub admin_api_host: String,
    pub admin_api_port: u16,
    pub ledger_api_host: String,
    pub ledger_api_port: u16,
    #[serde(default = "default_synchronizer")]
    pub synchronizer: String,
    /// Canton Network environment (devnet, testnet, mainnet)
    pub network: Network,
}

fn default_synchronizer() -> String {
    "global".to_string()
}

impl Default for CantonConfig {
    fn default() -> Self {
        Self {
            admin_api_host: "127.0.0.1".to_string(),
            admin_api_port: 5002,
            ledger_api_host: "127.0.0.1".to_string(),
            ledger_api_port: 5001,
            synchronizer: default_synchronizer(),
            network: Network::Devnet,
        }
    }
}

impl NodeConfig {
    /// Load node configuration from a root directory
    ///
    /// The directory should contain:
    /// - config/node.toml - Node configuration
    /// - config/peers.csv - Peers list (id,name,address,port,public_key,party)
    /// - data/noise.key - Noise keypair (auto-generated if missing)
    /// - data/workflow-data/ - Workflow state
    /// - data/dars/ - DAR files
    pub async fn from_dir<P: AsRef<Path>>(root_dir: P) -> Result<Self> {
        let root_dir = root_dir.as_ref();
        let config_path = root_dir.join(CONFIG_DIR).join(NODE_CONFIG_FILENAME);

        let mut config = if config_path.exists() {
            let content = tokio::fs::read_to_string(&config_path)
                .await
                .with_context(|| {
                    format!("Failed to read node config '{}'", config_path.display())
                })?;
            toml::from_str::<NodeConfig>(&content).with_context(|| {
                format!("Failed to parse node config '{}'", config_path.display())
            })?
        } else {
            tracing::info!("No node.toml found, using defaults");
            NodeConfig::default()
        };
        config.root_dir = root_dir.to_path_buf();
        Ok(config)
    }

    /// Get the config directory
    pub fn config_dir(&self) -> PathBuf {
        self.root_dir.join(CONFIG_DIR)
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

    /// Get the path to the peers.csv file
    pub fn peers_csv_path(&self) -> PathBuf {
        self.config_dir().join("peers.csv")
    }

    /// Load the peers configuration from peers.csv in the config directory
    pub async fn load_network_config(&self) -> Result<NetworkConfig> {
        NetworkConfig::from_file(&self.peers_csv_path()).await
    }

    /// Save the peers configuration to peers.csv in the config directory
    pub async fn save_network_config(&self, config: &NetworkConfig) -> Result {
        let peers_config_path = self.config_dir().join("peers.csv");
        config.save_to_file(&peers_config_path).await
    }

    /// Get the data directory
    pub fn data_dir(&self) -> PathBuf {
        self.root_dir.join(DATA_DIR)
    }

    /// Get the path to the noise key file
    pub fn key_file_path(&self) -> PathBuf {
        self.data_dir().join(NOISE_KEY_FILENAME)
    }

    /// Get the workflow data directory
    pub fn workflow_data_dir(&self) -> PathBuf {
        self.data_dir().join(WORKFLOW_DATA_DIR)
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

    /// Get party credentials by decentralized party ID
    pub fn get_party_credentials(&self, dec_party_id: &CantonId) -> Option<&PartyCredentials> {
        self.parties
            .iter()
            .find(|p| &p.dec_party_id == dec_party_id)
    }

    /// Get package config for a decentralized party
    pub fn get_packages(&self, dec_party_id: &str) -> PackageConfig {
        CantonId::parse(dec_party_id)
            .ok()
            .and_then(|id| self.get_party_credentials(&id))
            .map(|c| c.packages.clone())
            .unwrap_or_default()
    }

    /// Add or update party credentials in the config and save to disk
    pub async fn upsert_party_credentials(&mut self, creds: PartyCredentials) -> Result {
        if let Some(existing) = self
            .parties
            .iter_mut()
            .find(|p| p.dec_party_id == creds.dec_party_id)
        {
            *existing = creds;
        } else {
            self.parties.push(creds);
        }
        self.save_config().await
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

    /// Set the participant_id and save the config to disk
    pub async fn set_and_save_participant_id(&mut self, participant_id: CantonId) -> Result {
        self.node.participant_id = Some(participant_id);
        self.save_config().await
    }

    /// Save the current config to disk
    pub async fn save_config(&self) -> Result {
        let config_path = self.root_dir.join(CONFIG_DIR).join(NODE_CONFIG_FILENAME);

        // Create a serializable version without the root_dir field
        let toml_content =
            toml::to_string_pretty(self).with_context(|| "Failed to serialize config to TOML")?;

        tokio::fs::write(&config_path, toml_content)
            .await
            .with_context(|| format!("Failed to write config to '{}'", config_path.display()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer(index: u8, pub_key: &str) -> Peer {
        // Create a valid hex namespace (68 hex chars = 34 bytes)
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
    fn test_get_peer() {
        let peer1 = test_peer(1, "abc123");
        let peer1_id = peer1.participant_id.to_string();
        let network = NetworkConfig { peers: vec![peer1] };

        assert!(network.get_peer(&peer1_id).is_some());
        assert!(network.get_peer("nonexistent").is_none());
    }

    #[test]
    fn test_public_key_allowlist() {
        let peer1 = test_peer(1, "abc123");
        let peer2 = test_peer(2, "def456");
        let peer1_id = peer1.participant_id.to_string();
        let peer2_id = peer2.participant_id.to_string();
        let network = NetworkConfig {
            peers: vec![peer1, peer2],
        };

        let allowlist = network.get_public_key_allowlist();
        assert_eq!(allowlist.len(), 2);
        assert_eq!(allowlist.get("abc123"), Some(&peer1_id));
        assert_eq!(allowlist.get("def456"), Some(&peer2_id));
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

    const TEST_NAMESPACE: &str =
        "1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    const MINIMAL_NODE_TOML: &str = r#"
[node]
listen_address = "0.0.0.0"
port = 9000

[canton]
admin_api_host = "localhost"
admin_api_port = 5002
ledger_api_host = "localhost"
ledger_api_port = 5001
network = "devnet"
"#;

    fn test_party_creds(prefix: &str) -> PartyCredentials {
        PartyCredentials {
            dec_party_id: CantonId::parse(&format!("{prefix}::{TEST_NAMESPACE}")).unwrap(),
            member_party_id: CantonId::parse(&format!("member::{TEST_NAMESPACE}")).unwrap(),
            user_id: "TestUser".to_string(),
            keycloak: KeycloakConfig {
                url: "https://kc.example.com".to_string(),
                realm: "test".to_string(),
                client_id: "client-1".to_string(),
                client_secret: None,
                username: None,
                password: None,
            },
            packages: PackageConfig::default(),
        }
    }

    async fn temp_node_config() -> (tempfile::TempDir, NodeConfig) {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("node.toml"), MINIMAL_NODE_TOML).unwrap();
        let config = NodeConfig::from_dir(tmp.path()).await.unwrap();
        (tmp, config)
    }

    #[tokio::test]
    async fn test_upsert_party_credentials_insert() {
        let (tmp, mut config) = temp_node_config().await;
        assert!(config.parties.is_empty());

        let creds = test_party_creds("new-party");
        config
            .upsert_party_credentials(creds.clone())
            .await
            .unwrap();
        assert_eq!(config.parties.len(), 1);
        assert_eq!(config.parties[0].user_id, "TestUser");

        let reloaded = NodeConfig::from_dir(tmp.path()).await.unwrap();
        assert_eq!(reloaded.parties.len(), 1);
        assert_eq!(reloaded.parties[0].dec_party_id, creds.dec_party_id);
    }

    #[tokio::test]
    async fn test_upsert_party_credentials_update() {
        let (_tmp, mut config) = temp_node_config().await;

        let creds = test_party_creds("my-party");
        config.upsert_party_credentials(creds).await.unwrap();
        assert_eq!(config.parties[0].user_id, "TestUser");

        let mut updated = test_party_creds("my-party");
        updated.user_id = "UpdatedUser".to_string();
        config.upsert_party_credentials(updated).await.unwrap();
        assert_eq!(config.parties.len(), 1);
        assert_eq!(config.parties[0].user_id, "UpdatedUser");
    }

    #[test]
    fn test_default_package_config() {
        let packages = default_package_config();
        assert_eq!(
            packages.governance_core.as_deref(),
            Some("#governance-core-v0-rc1"),
        );
        assert_eq!(
            packages.governance_token_custody.as_deref(),
            Some("#governance-token-custody-v0-rc1"),
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
