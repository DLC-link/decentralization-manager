use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::api::AuthSettings;
use crate::auth::PasswordSource;

/// Optional multi-profile config file, read from the current directory unless
/// `--config` names another path.
pub const CONFIG_FILE: &str = "config.toml";

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
    /// The password itself. Set this or `password_command`, not both.
    #[serde(default)]
    pub password: Option<String>,
    /// A shell command that prints the password, for example a password
    /// manager CLI. It runs only when a password login is needed.
    #[serde(default)]
    pub password_command: Option<String>,
    /// When `true`, commands that change state need `--yes`.
    #[serde(default)]
    pub require_yes: bool,
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
    ///
    /// # Errors
    ///
    /// Returns an error if the profile sets both `password` and
    /// `password_command`.
    pub fn auth(&self) -> Result<AuthSettings> {
        let non_empty = |value: &Option<String>| value.clone().filter(|s| !s.is_empty());
        Ok(AuthSettings {
            username: self.username.clone(),
            password: password_source(
                &self.name,
                non_empty(&self.password),
                non_empty(&self.password_command),
            )?,
            token_url: non_empty(&self.oauth_token_url),
            client_id: non_empty(&self.oauth_client_id),
            client_secret: non_empty(&self.oauth_client_secret),
            audience: non_empty(&self.oauth_audience),
            scope: non_empty(&self.oauth_scope),
        })
    }
}

/// The password source from a literal password or a password command, set in
/// the login named `login`.
///
/// # Errors
///
/// Returns an error if both are set.
pub fn password_source(
    login: &str,
    password: Option<String>,
    password_command: Option<String>,
) -> Result<Option<PasswordSource>> {
    match (password, password_command) {
        (Some(_), Some(_)) => {
            bail!("{login}: set password or password_command, not both")
        }
        (Some(password), None) => Ok(Some(PasswordSource::Literal(password))),
        (None, Some(command)) => Ok(Some(PasswordSource::Command(command))),
        (None, None) => Ok(None),
    }
}

/// Top-level `config.toml` shape: a list of `[[profiles]]`.
#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    profiles: Vec<Profile>,
}

/// Load login profiles from `path`. Returns an empty vec when the file is
/// absent — the caller then falls back to `.env`.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn load_profiles(path: &Path) -> Result<Vec<Profile>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let config: ConfigFile =
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
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

        let auth = config.profiles[1].auth().unwrap();
        assert_eq!(auth.username, "bob");
        assert!(matches!(auth.password, Some(PasswordSource::Literal(ref p)) if p == "hunter2"));
        assert_eq!(auth.client_id.as_deref(), Some("decman-cli"));
        assert_eq!(auth.token_url, None);
    }

    #[test]
    fn password_command_profile_needs_no_password() {
        // Arrange
        let toml = r#"
            [[profiles]]
            name = "Mainnet"
            api_url = "https://decman.example"
            username = "carol"
            password_command = "pass show decman"
            require_yes = true
        "#;

        // Act
        let config: ConfigFile = toml::from_str(toml).unwrap();
        let profile = &config.profiles[0];
        let auth = profile.auth().unwrap();

        // Assert
        assert!(profile.require_yes);
        assert!(
            matches!(auth.password, Some(PasswordSource::Command(ref c)) if c == "pass show decman")
        );
    }

    #[test]
    fn profile_with_password_and_command_is_rejected() {
        // Arrange
        let toml = r#"
            [[profiles]]
            name = "Both"
            api_url = "https://decman.example"
            username = "dave"
            password = "x"
            password_command = "echo x"
        "#;

        // Act
        let config: ConfigFile = toml::from_str(toml).unwrap();
        let error = config.profiles[0].auth().unwrap_err();

        // Assert
        assert!(error.to_string().contains("not both"), "{error}");
    }
}
