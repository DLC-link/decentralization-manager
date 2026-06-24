use clap::Parser;

use crate::api::AuthSettings;

/// Command-line / environment configuration for decman-cli.
///
/// Every field has an `env` fallback, and `.env` is loaded before parsing, so a
/// `.env` file holding the API URL and your frontend username/password is
/// enough to run. The IdP token endpoint and client id are discovered from the
/// API's `/auth-config` unless overridden.
#[derive(Debug, Parser)]
#[command(
    name = "decman-cli",
    version,
    about = "Terminal UI for the BitSafe Decentralization Manager"
)]
pub struct Cli {
    /// Base URL of the decman API (for example `http://localhost:8081`).
    #[arg(long, env = "DECMAN_API_URL")]
    pub api_url: String,

    /// Frontend username to log in with.
    #[arg(long, env = "DECMAN_USERNAME")]
    pub username: String,

    /// Frontend password to log in with.
    #[arg(long, env = "DECMAN_PASSWORD", hide_env_values = true)]
    pub password: String,

    /// OAuth2 token endpoint (auto-discovered from `/auth-config` if unset).
    #[arg(long, env = "OAUTH_TOKEN_URL")]
    pub oauth_token_url: Option<String>,

    /// OAuth2 client id (auto-discovered from `/auth-config` if unset).
    #[arg(long, env = "OAUTH_CLIENT_ID")]
    pub oauth_client_id: Option<String>,

    /// OAuth2 client secret, for confidential clients only.
    #[arg(long, env = "OAUTH_CLIENT_SECRET", hide_env_values = true)]
    pub oauth_client_secret: Option<String>,

    /// OAuth2 audience (required by some IdPs, e.g. Auth0).
    #[arg(long, env = "OAUTH_AUDIENCE")]
    pub oauth_audience: Option<String>,

    /// OAuth2 scope(s), space-separated.
    #[arg(long, env = "OAUTH_SCOPE")]
    pub oauth_scope: Option<String>,
}

impl Cli {
    /// Build the password-grant auth settings, treating empty optional env vars
    /// as unset.
    pub fn auth_settings(&self) -> AuthSettings {
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
