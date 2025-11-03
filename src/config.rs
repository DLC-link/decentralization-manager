use std::path::Path;

use serde::Deserialize;

use crate::error::Result;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub connection: ConnectionConfig,
    pub topology: TopologyConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConnectionConfig {
    pub admin_api_host: String,
    pub admin_api_port: u16,
    pub ledger_api_host: String,
    pub ledger_api_port: u16,
    pub token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopologyConfig {
    pub synchronizer: String,
}

impl Config {
    /// Load configuration from a TOML file
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = tokio::fs::read_to_string(path.as_ref()).await?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Get the full Admin API URL
    pub fn admin_api_url(&self) -> String {
        format!(
            "http://{}:{}",
            self.connection.admin_api_host, self.connection.admin_api_port
        )
    }

    /// Get the full Ledger API URL
    pub fn ledger_api_url(&self) -> String {
        format!(
            "http://{}:{}",
            self.connection.ledger_api_host, self.connection.ledger_api_port
        )
    }

    /// Get the authorization token if present
    pub fn auth_token(&self) -> Option<&str> {
        self.connection.token.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_urls() {
        let config = Config {
            connection: ConnectionConfig {
                admin_api_host: "localhost".to_string(),
                admin_api_port: 5001,
                ledger_api_host: "localhost".to_string(),
                ledger_api_port: 5002,
                token: Some("test_token".to_string()),
            },
            topology: TopologyConfig {
                synchronizer: "global".to_string(),
            },
        };

        assert_eq!(config.admin_api_url(), "http://localhost:5001");
        assert_eq!(config.ledger_api_url(), "http://localhost:5002");
        assert_eq!(config.auth_token(), Some("test_token"));
    }
}
