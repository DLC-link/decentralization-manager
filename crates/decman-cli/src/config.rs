use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::api::AuthSettings;

/// Optional multi-profile config file, read from the current directory.
const CONFIG_FILE: &str = "config.toml";

/// Remembers the last-selected profile across launches (in the current dir).
const SESSION_FILE: &str = ".decman-cli-session";

/// One login profile from `config.toml`.
#[derive(Clone, Debug, Deserialize)]
pub struct Profile {
    /// Custom display name for this profile.
    pub name: String,
    /// Network label (e.g. `localnet` / `devnet`), shown in the menu.
    #[serde(default)]
    pub network: String,
    pub api_url: String,
    pub username: String,
    pub password: String,
    /// Optional OAuth overrides (auto-discovered from `/auth-config` if unset).
    #[serde(default)]
    pub oauth_token_url: Option<String>,
    #[serde(default)]
    pub oauth_client_id: Option<String>,
    #[serde(default)]
    pub oauth_client_secret: Option<String>,
    #[serde(default)]
    pub oauth_audience: Option<String>,
    #[serde(default)]
    pub oauth_scope: Option<String>,
}

impl Profile {
    /// Auth settings for this profile, treating empty optional fields as unset.
    pub fn auth(&self) -> AuthSettings {
        let non_empty = |value: &Option<String>| value.clone().filter(|s| !s.is_empty());
        AuthSettings {
            username: self.username.clone(),
            password: self.password.clone(),
            token_url: non_empty(&self.oauth_token_url),
            client_id: non_empty(&self.oauth_client_id),
            client_secret: non_empty(&self.oauth_client_secret),
            audience: non_empty(&self.oauth_audience),
            scope: non_empty(&self.oauth_scope),
        }
    }
}

/// Top-level `config.toml` shape: a list of `[[profiles]]`.
#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    profiles: Vec<Profile>,
}

/// Load login profiles from `config.toml`. Returns an empty vec when the file
/// is absent — the caller then falls back to `.env`.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn load_profiles() -> Result<Vec<Profile>> {
    let path = Path::new(CONFIG_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).context("failed to read config.toml")?;
    let config: ConfigFile = toml::from_str(&text).context("failed to parse config.toml")?;
    Ok(config.profiles)
}

/// The remembered profile name from a previous session, if any.
pub fn remembered_profile() -> Option<String> {
    std::fs::read_to_string(SESSION_FILE)
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// Remember `name` as the active profile across launches.
pub fn remember_profile(name: &str) {
    let _ = std::fs::write(SESSION_FILE, name);
}

/// Forget the remembered profile (on logout).
pub fn forget_profile() {
    let _ = std::fs::remove_file(SESSION_FILE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_profiles() {
        // Arrange
        let toml = r#"
            [[profiles]]
            name = "Localnet"
            network = "localnet"
            api_url = "http://localhost:8081"
            username = "alice"
            password = "secret"

            [[profiles]]
            name = "Devnet"
            api_url = "https://decman.devnet"
            username = "bob"
            password = "hunter2"
            oauth_client_id = "decman-cli"
        "#;

        // Act
        let config: ConfigFile = toml::from_str(toml).unwrap();

        // Assert
        assert_eq!(config.profiles.len(), 2);
        assert_eq!(config.profiles[0].name, "Localnet");
        assert_eq!(config.profiles[0].network, "localnet");
        // `network` defaults to empty when omitted.
        assert_eq!(config.profiles[1].network, "");

        let auth = config.profiles[1].auth();
        assert_eq!(auth.username, "bob");
        assert_eq!(auth.password, "hunter2");
        assert_eq!(auth.client_id.as_deref(), Some("decman-cli"));
        assert_eq!(auth.token_url, None);
    }
}
