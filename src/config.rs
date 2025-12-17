use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::{
    consts::{
        CONFIG_DIR, DARS_DIR, DATA_DIR, NODE_CONFIG_FILENAME, NOISE_KEY_FILENAME, WORKFLOW_DATA_DIR,
    },
    error::Result,
};

/// Network configuration - list of peers in the network
#[derive(Clone, Debug, Default, Serialize)]
pub struct NetworkConfig {
    /// List of peers in the network
    pub peers: Vec<Peer>,
}

/// A peer in the network
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Peer {
    /// Unique identifier for the peer (e.g., "participant-1")
    pub id: String,
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
    /// Load network configuration from a CSV file
    ///
    /// CSV format: id,name,address,port,public_key,party
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
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
    pub async fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
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

    /// Get peer by ID
    pub fn get_peer(&self, id: &str) -> Option<&Peer> {
        self.peers.iter().find(|p| p.id == id)
    }

    /// Create a map of public keys to peer IDs for verification
    pub fn get_public_key_allowlist(&self) -> HashMap<String, String> {
        self.peers
            .iter()
            .map(|p| (p.public_key.clone(), p.id.clone()))
            .collect()
    }
}

/// Timeout configuration
#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeConfig {
    pub node: NodeInfo,
    pub canton: CantonConfig,
    #[serde(default)]
    pub timeouts: Timeouts,
    /// Root directory containing config/ and data/ subdirectories
    #[serde(skip)]
    root_dir: PathBuf,
}

/// Node-specific information
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeInfo {
    /// Unique identifier for this node (used for peer identification)
    pub node_id: String,
    /// Address to listen on for Noise protocol connections
    #[serde(default = "default_listen_address")]
    pub listen_address: String,
    /// Port to listen on for Noise protocol connections
    #[serde(default = "default_noise_port")]
    pub port: u16,
}

fn default_listen_address() -> String {
    "0.0.0.0".to_string()
}

fn default_noise_port() -> u16 {
    9000
}

/// Canton participant configuration
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CantonConfig {
    pub admin_api_host: String,
    pub admin_api_port: u16,
    pub ledger_api_host: String,
    pub ledger_api_port: u16,
    #[serde(default = "default_synchronizer")]
    pub synchronizer: String,
    /// Optional JWT token for Ledger API authentication
    /// If not provided, requests will be sent without authentication
    #[serde(default)]
    pub ledger_api_token: Option<String>,
    /// Ledger API user ID for submission operations
    /// Must match the JWT token's "sub" claim
    pub ledger_api_user_id: String,
}

fn default_synchronizer() -> String {
    "global".to_string()
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

        let content = tokio::fs::read_to_string(&config_path)
            .await
            .with_context(|| format!("Failed to read node config '{}'", config_path.display()))?;
        let mut config: NodeConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse node config '{}'", config_path.display()))?;
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

    /// Load the peers configuration from peers.csv in the config directory
    pub async fn load_network_config(&self) -> Result<NetworkConfig> {
        let peers_config_path = self.config_dir().join("peers.csv");
        NetworkConfig::from_file(&peers_config_path).await
    }

    /// Save the peers configuration to peers.csv in the config directory
    pub async fn save_network_config(&self, config: &NetworkConfig) -> Result<()> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_governance_threshold() {
        let network = NetworkConfig {
            peers: vec![
                Peer {
                    id: "node1".to_string(),
                    name: "Node 1".to_string(),
                    address: "10.0.1.1".to_string(),
                    port: 9000,
                    public_key: "abc123".to_string(),
                    party: None,
                },
                Peer {
                    id: "node2".to_string(),
                    name: "Node 2".to_string(),
                    address: "10.0.1.2".to_string(),
                    port: 9000,
                    public_key: "def456".to_string(),
                    party: None,
                },
                Peer {
                    id: "node3".to_string(),
                    name: "Node 3".to_string(),
                    address: "10.0.1.3".to_string(),
                    port: 9000,
                    public_key: "ghi789".to_string(),
                    party: None,
                },
            ],
        };

        assert_eq!(network.governance_threshold(), 2);
    }

    #[test]
    fn test_get_peer() {
        let network = NetworkConfig {
            peers: vec![Peer {
                id: "node1".to_string(),
                name: "Node 1".to_string(),
                address: "10.0.1.1".to_string(),
                port: 9000,
                public_key: "abc123".to_string(),
                party: None,
            }],
        };

        assert!(network.get_peer("node1").is_some());
        assert!(network.get_peer("nonexistent").is_none());
    }

    #[test]
    fn test_public_key_allowlist() {
        let network = NetworkConfig {
            peers: vec![
                Peer {
                    id: "node1".to_string(),
                    name: "Node 1".to_string(),
                    address: "10.0.1.1".to_string(),
                    port: 9000,
                    public_key: "abc123".to_string(),
                    party: None,
                },
                Peer {
                    id: "node2".to_string(),
                    name: "Node 2".to_string(),
                    address: "10.0.1.2".to_string(),
                    port: 9000,
                    public_key: "def456".to_string(),
                    party: None,
                },
            ],
        };

        let allowlist = network.get_public_key_allowlist();
        assert_eq!(allowlist.len(), 2);
        assert_eq!(allowlist.get("abc123"), Some(&"node1".to_string()));
        assert_eq!(allowlist.get("def456"), Some(&"node2".to_string()));
    }
}
