use std::{collections::HashMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Coordinator selection strategy
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
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
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ParticipantRole {
    Coordinator,
    Attestor,
}

/// Network-wide configuration shared by all participants
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfig {
    pub network: NetworkInfo,
    pub participants: Vec<Participant>,
    #[serde(default)]
    pub timeouts: Timeouts,
    #[serde(default)]
    pub security: SecurityConfig,
}

/// Basic network information
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkInfo {
    pub name: String,
    pub protocol_version: String,
    pub port: u16,
    pub coordinator_strategy: CoordinatorStrategy,
}

/// Participant in the network
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Participant {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<ParticipantRole>,
    pub address: String,
    pub port: u16,
    /// Hex-encoded Noise static public key
    pub public_key: String,
}

/// Timeout configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
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

/// Security configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecurityConfig {
    #[serde(default = "default_require_all")]
    pub require_all_participants: bool,
    #[serde(default = "default_minimum_participants")]
    pub minimum_participants: usize,
}

fn default_require_all() -> bool {
    true
}
fn default_minimum_participants() -> usize {
    3
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            require_all_participants: default_require_all(),
            minimum_participants: default_minimum_participants(),
        }
    }
}

impl NetworkConfig {
    /// Load network configuration from a TOML file
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = tokio::fs::read_to_string(path.as_ref()).await?;
        let config: NetworkConfig = toml::from_str(&content)?;
        Ok(config)
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

    /// Check if a participant ID is the coordinator
    pub fn is_coordinator(&self, participant_id: &str) -> Result<bool> {
        let coordinator = self.get_coordinator()?;
        Ok(coordinator.id == participant_id)
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
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NodeConfig {
    pub node: NodeInfo,
    pub network_config: String,
    pub canton: CantonConfig,
}

/// Node-specific information
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NodeInfo {
    /// Must match one of the participant IDs in network.toml
    pub participant_id: String,
    /// Path to this node's static private key
    pub static_key_file: String,
    #[serde(default = "default_listen_address")]
    pub listen_address: String,
}

fn default_listen_address() -> String {
    "0.0.0.0".to_string()
}

/// Canton participant configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CantonConfig {
    pub admin_api_host: String,
    pub admin_api_port: u16,
    pub ledger_api_host: String,
    pub ledger_api_port: u16,
    pub token: Option<String>,
    #[serde(default = "default_synchronizer")]
    pub synchronizer: String,
}

fn default_synchronizer() -> String {
    "global".to_string()
}

impl NodeConfig {
    /// Load node configuration from a TOML file
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = tokio::fs::read_to_string(path.as_ref()).await?;
        let config: NodeConfig = toml::from_str(&content)?;
        Ok(config)
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

    /// Get the authorization token if present
    pub fn auth_token(&self) -> Option<&str> {
        self.canton.token.as_deref()
    }

    /// Get the synchronizer name
    pub fn synchronizer(&self) -> &str {
        &self.canton.synchronizer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coordinator_strategy_explicit() {
        let network = NetworkConfig {
            network: NetworkInfo {
                name: "test".to_string(),
                protocol_version: "1.0".to_string(),
                port: 9000,
                coordinator_strategy: CoordinatorStrategy::Explicit,
            },
            participants: vec![
                Participant {
                    id: "node1".to_string(),
                    name: "Node 1".to_string(),
                    role: Some(ParticipantRole::Coordinator),
                    address: "10.0.1.1".to_string(),
                    port: 9000,
                    public_key: "abc123".to_string(),
                },
                Participant {
                    id: "node2".to_string(),
                    name: "Node 2".to_string(),
                    role: None,
                    address: "10.0.1.2".to_string(),
                    port: 9000,
                    public_key: "def456".to_string(),
                },
            ],
            timeouts: Timeouts::default(),
            security: SecurityConfig::default(),
        };

        let coordinator = network.get_coordinator().unwrap();
        assert_eq!(coordinator.id, "node1");
        assert!(network.is_coordinator("node1").unwrap());
        assert!(!network.is_coordinator("node2").unwrap());
    }

    #[test]
    fn test_coordinator_strategy_first() {
        let network = NetworkConfig {
            network: NetworkInfo {
                name: "test".to_string(),
                protocol_version: "1.0".to_string(),
                port: 9000,
                coordinator_strategy: CoordinatorStrategy::First,
            },
            participants: vec![
                Participant {
                    id: "node1".to_string(),
                    name: "Node 1".to_string(),
                    role: None,
                    address: "10.0.1.1".to_string(),
                    port: 9000,
                    public_key: "abc123".to_string(),
                },
                Participant {
                    id: "node2".to_string(),
                    name: "Node 2".to_string(),
                    role: None,
                    address: "10.0.1.2".to_string(),
                    port: 9000,
                    public_key: "def456".to_string(),
                },
            ],
            timeouts: Timeouts::default(),
            security: SecurityConfig::default(),
        };

        let coordinator = network.get_coordinator().unwrap();
        assert_eq!(coordinator.id, "node1");
    }

    #[test]
    fn test_get_attestors() {
        let network = NetworkConfig {
            network: NetworkInfo {
                name: "test".to_string(),
                protocol_version: "1.0".to_string(),
                port: 9000,
                coordinator_strategy: CoordinatorStrategy::First,
            },
            participants: vec![
                Participant {
                    id: "node1".to_string(),
                    name: "Node 1".to_string(),
                    role: None,
                    address: "10.0.1.1".to_string(),
                    port: 9000,
                    public_key: "abc123".to_string(),
                },
                Participant {
                    id: "node2".to_string(),
                    name: "Node 2".to_string(),
                    role: None,
                    address: "10.0.1.2".to_string(),
                    port: 9000,
                    public_key: "def456".to_string(),
                },
                Participant {
                    id: "node3".to_string(),
                    name: "Node 3".to_string(),
                    role: None,
                    address: "10.0.1.3".to_string(),
                    port: 9000,
                    public_key: "ghi789".to_string(),
                },
            ],
            timeouts: Timeouts::default(),
            security: SecurityConfig::default(),
        };

        let attestors = network.get_attestors().unwrap();
        assert_eq!(attestors.len(), 2);
        assert_eq!(attestors[0].id, "node2");
        assert_eq!(attestors[1].id, "node3");
    }
}
