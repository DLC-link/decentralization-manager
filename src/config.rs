use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    consts::{DARS_DIR, DATA_DIR, KEYS_DIR, WORKFLOW_DATA_DIR},
    error::Result,
};

/// Coordinator selection strategy
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CoordinatorStrategy {
    /// Explicitly designated coordinator (via role field)
    Explicit,
    /// First participant in the list becomes coordinator
    First,
    /// Leader election using Bully algorithm
    Election,
}

/// Role of a participant in the network
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ParticipantRole {
    Coordinator,
    Attestor,
}

/// Network-wide configuration shared by all participants
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NetworkConfig {
    pub network: NetworkInfo,
    pub participants: Vec<Participant>,
    #[serde(default)]
    pub timeouts: Timeouts,
    /// Application-specific configuration (contracts, party prefixes, etc.)
    pub application: ApplicationConfig,
}

/// Application-specific configuration for the decentralized party
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ApplicationConfig {
    /// Party ID prefix used for constructing decentralized party identifiers
    /// Format: "{party_id_prefix}::<namespace>"
    pub party_id_prefix: String,
    /// Name prefix for namespace signing keys
    pub namespace_key_name: String,
    /// Name prefix for DAML transaction signing keys
    pub daml_key_name: String,
    /// Party hint for operator party allocation
    #[serde(default = "default_operator_party_hint")]
    pub operator_party_hint: String,
    /// Contract definitions to create after decentralized party setup
    #[serde(default)]
    pub contracts: Vec<ContractDefinition>,
}

fn default_operator_party_hint() -> String {
    "operator".to_string()
}

/// Definition of a Daml contract to create on the ledger
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContractDefinition {
    /// Unique identifier for this contract (used as command ID)
    pub id: String,
    /// Human-readable name for logging
    pub name: String,
    /// Package ID (can use # prefix for symbolic lookup)
    pub package_id: String,
    /// Module name (e.g., "CBTC.Governance")
    pub module_name: String,
    /// Entity/template name (e.g., "CBTCGovernanceRules")
    pub entity_name: String,
    /// Record fields for the create command
    pub fields: Vec<FieldDefinition>,
}

/// Definition of a field value in a Daml record
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldDefinition {
    /// The decentralized party ID
    DecentralizedParty,
    /// The operator party ID
    OperatorParty,
    /// A specific participant's party ID (0-indexed)
    ParticipantParty { index: usize },
    /// Static text value
    Text { value: String },
    /// Integer value
    Int64 { value: i64 },
    /// Boolean value
    Bool { value: bool },
    /// The instrument record (admin party + instrument id)
    Instrument { id: String },
    /// Set of all participant parties (as GenMap<Party, Unit>)
    AttestorsSet,
    /// Optional wrapper around another field
    Optional { inner: Box<FieldDefinition> },
    /// Nested record with fields
    Record { fields: Vec<FieldDefinition> },
    /// Governance threshold (calculated from participant count)
    GovernanceThreshold,
}

/// Basic network information
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NetworkInfo {
    pub name: String,
    pub protocol_version: String,
    pub coordinator_strategy: CoordinatorStrategy,
    /// Operator party ID (optional, can be allocated dynamically if not provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_party: Option<String>,
}

/// Participant in the network
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Participant {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<ParticipantRole>,
    pub address: String,
    pub port: u16,
    /// Hex-encoded Noise static public key
    pub public_key: String,
    /// Canton party ID (optional, can be allocated dynamically if not provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub party: Option<String>,
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

impl NetworkConfig {
    /// Load network configuration from a TOML file
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        use anyhow::Context;

        let path = path.as_ref();
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read network config '{}'", path.display()))?;
        let config: NetworkConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse network config '{}'", path.display()))?;
        Ok(config)
    }

    /// Get the governance threshold for multi-sig operations
    /// Returns majority threshold: (n/2 + 1)
    pub fn governance_threshold(&self) -> u32 {
        ((self.participants.len() / 2) + 1) as u32
    }

    /// Get participant by ID
    pub fn get_participant(&self, id: &str) -> Option<&Participant> {
        self.participants.iter().find(|p| p.id == id)
    }

    /// Get the coordinator based on the strategy
    pub fn get_coordinator(&self) -> Result<&Participant> {
        match self.network.coordinator_strategy {
            CoordinatorStrategy::Explicit => {
                // Find participant with coordinator role
                self.participants
                    .iter()
                    .find(|p| p.role == Some(ParticipantRole::Coordinator))
                    .ok_or_else(|| {
                        anyhow::anyhow!("No coordinator found with explicit coordinator role")
                    })
            }
            CoordinatorStrategy::First => {
                // First participant is coordinator
                self.participants.first().ok_or_else(|| {
                    anyhow::anyhow!("No participants defined in network configuration")
                })
            }
            CoordinatorStrategy::Election => {
                // For election, we need runtime state, so this will be handled elsewhere
                // For now, fall back to explicit or first
                if let Some(coordinator) = self
                    .participants
                    .iter()
                    .find(|p| p.role == Some(ParticipantRole::Coordinator))
                {
                    Ok(coordinator)
                } else {
                    self.participants.first().ok_or_else(|| {
                        anyhow::anyhow!("No participants defined in network configuration")
                    })
                }
            }
        }
    }

    /// Check if a node ID is the coordinator
    pub fn is_coordinator(&self, node_id: &str) -> Result<bool> {
        let coordinator = self.get_coordinator()?;
        Ok(coordinator.id == node_id)
    }

    /// Get all attestors (non-coordinator participants)
    pub fn get_attestors(&self) -> Result<Vec<&Participant>> {
        let coordinator = self.get_coordinator()?;
        Ok(self
            .participants
            .iter()
            .filter(|p| p.id != coordinator.id)
            .collect())
    }

    /// Create a map of public keys to participant IDs for verification
    pub fn get_public_key_allowlist(&self) -> HashMap<String, String> {
        self.participants
            .iter()
            .map(|p| (p.public_key.clone(), p.id.clone()))
            .collect()
    }
}

/// Individual node configuration
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeConfig {
    pub node: NodeInfo,
    pub network_config: String,
    pub canton: CantonConfig,
    #[serde(skip)]
    config_dir: PathBuf,
}

/// Node-specific information
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeInfo {
    /// Must match one of the participant IDs in network.toml
    pub node_id: String,
    #[serde(default = "default_listen_address")]
    pub listen_address: String,
}

fn default_listen_address() -> String {
    "0.0.0.0".to_string()
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
    /// Load node configuration from a TOML file
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        use anyhow::Context;

        let path = path.as_ref();
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read node config '{}'", path.display()))?;
        let mut config: NodeConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse node config '{}'", path.display()))?;
        config.config_dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        Ok(config)
    }

    /// Get the full Admin API URL
    pub fn admin_api_url(&self) -> String {
        format!(
            "http://{host}:{port}",
            host = self.canton.admin_api_host,
            port = self.canton.admin_api_port
        )
    }

    /// Get the full Ledger API URL
    pub fn ledger_api_url(&self) -> String {
        format!(
            "http://{host}:{port}",
            host = self.canton.ledger_api_host,
            port = self.canton.ledger_api_port
        )
    }

    /// Get the synchronizer name
    pub fn synchronizer(&self) -> &str {
        &self.canton.synchronizer
    }

    /// Load the associated network configuration
    pub async fn load_network_config(&self) -> Result<NetworkConfig> {
        let network_config_path = if PathBuf::from(&self.network_config).is_absolute() {
            PathBuf::from(&self.network_config)
        } else {
            self.config_dir.join(&self.network_config)
        };
        NetworkConfig::from_file(&network_config_path).await
    }

    /// Get the data directory (sibling to the config directory)
    pub fn data_dir(&self) -> PathBuf {
        self.config_dir
            .parent()
            .unwrap_or(&self.config_dir)
            .join(DATA_DIR)
    }

    /// Get the keys directory
    pub fn keys_dir(&self) -> PathBuf {
        self.data_dir().join(KEYS_DIR)
    }

    /// Get the path to this node's key file
    pub fn key_file_path(&self) -> PathBuf {
        self.keys_dir().join(format!("{}.key", self.node.node_id))
    }

    /// Get the workflow data directory
    pub fn workflow_data_dir(&self) -> PathBuf {
        self.data_dir().join(WORKFLOW_DATA_DIR)
    }

    /// Get the dars directory (sibling to the config directory)
    pub fn dars_dir(&self) -> PathBuf {
        self.config_dir
            .parent()
            .unwrap_or(&self.config_dir)
            .join(DARS_DIR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_application_config() -> ApplicationConfig {
        ApplicationConfig {
            party_id_prefix: "test-network".to_string(),
            namespace_key_name: "test-namespace".to_string(),
            daml_key_name: "test-daml".to_string(),
            operator_party_hint: "operator".to_string(),
            contracts: vec![],
        }
    }

    #[test]
    fn test_coordinator_strategy_explicit() -> Result {
        let network = NetworkConfig {
            network: NetworkInfo {
                name: "test".to_string(),
                protocol_version: "1.0".to_string(),
                coordinator_strategy: CoordinatorStrategy::Explicit,
                operator_party: None,
            },
            participants: vec![
                Participant {
                    id: "node1".to_string(),
                    name: "Node 1".to_string(),
                    role: Some(ParticipantRole::Coordinator),
                    address: "10.0.1.1".to_string(),
                    port: 9000,
                    public_key: "abc123".to_string(),
                    party: None,
                },
                Participant {
                    id: "node2".to_string(),
                    name: "Node 2".to_string(),
                    role: None,
                    address: "10.0.1.2".to_string(),
                    port: 9000,
                    public_key: "def456".to_string(),
                    party: None,
                },
            ],
            timeouts: Timeouts::default(),
            application: test_application_config(),
        };

        let coordinator = network.get_coordinator()?;
        assert_eq!(coordinator.id, "node1");
        assert!(network.is_coordinator("node1")?);
        assert!(!network.is_coordinator("node2")?);
        Ok(())
    }

    #[test]
    fn test_coordinator_strategy_first() -> Result {
        let network = NetworkConfig {
            network: NetworkInfo {
                name: "test".to_string(),
                protocol_version: "1.0".to_string(),
                coordinator_strategy: CoordinatorStrategy::First,
                operator_party: None,
            },
            participants: vec![
                Participant {
                    id: "node1".to_string(),
                    name: "Node 1".to_string(),
                    role: None,
                    address: "10.0.1.1".to_string(),
                    port: 9000,
                    public_key: "abc123".to_string(),
                    party: None,
                },
                Participant {
                    id: "node2".to_string(),
                    name: "Node 2".to_string(),
                    role: None,
                    address: "10.0.1.2".to_string(),
                    port: 9000,
                    public_key: "def456".to_string(),
                    party: None,
                },
            ],
            timeouts: Timeouts::default(),
            application: test_application_config(),
        };

        let coordinator = network.get_coordinator()?;
        assert_eq!(coordinator.id, "node1");
        Ok(())
    }

    #[test]
    fn test_get_attestors() -> Result {
        let network = NetworkConfig {
            network: NetworkInfo {
                name: "test".to_string(),
                protocol_version: "1.0".to_string(),
                coordinator_strategy: CoordinatorStrategy::First,
                operator_party: None,
            },
            participants: vec![
                Participant {
                    id: "node1".to_string(),
                    name: "Node 1".to_string(),
                    role: None,
                    address: "10.0.1.1".to_string(),
                    port: 9000,
                    public_key: "abc123".to_string(),
                    party: None,
                },
                Participant {
                    id: "node2".to_string(),
                    name: "Node 2".to_string(),
                    role: None,
                    address: "10.0.1.2".to_string(),
                    port: 9000,
                    public_key: "def456".to_string(),
                    party: None,
                },
                Participant {
                    id: "node3".to_string(),
                    name: "Node 3".to_string(),
                    role: None,
                    address: "10.0.1.3".to_string(),
                    port: 9000,
                    public_key: "ghi789".to_string(),
                    party: None,
                },
            ],
            timeouts: Timeouts::default(),
            application: test_application_config(),
        };

        let attestors = network.get_attestors()?;
        assert_eq!(attestors.len(), 2, "Expected 2 attestors in test config");
        assert_eq!(attestors[0].id, "node2");
        assert_eq!(attestors[1].id, "node3");
        Ok(())
    }
}
