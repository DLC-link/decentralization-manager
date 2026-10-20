use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser};

use crate::api::AuthSettings;
use crate::command::Command;
use crate::config::password_source;

/// Command-line / environment configuration for decman-cli.
///
/// Without a command, decman-cli opens the terminal UI. With a command, it
/// runs that one action, prints JSON on stdout, and exits.
///
/// Every connection field has an `env` fallback, and `.env` is loaded before
/// parsing, so a `.env` file holding the API URL and your frontend
/// username/password is enough to run. The IdP token endpoint and client id
/// are discovered from the API's `/auth-config` unless overridden.
#[derive(Debug, Parser)]
#[command(
    name = "decman-cli",
    version,
    about = "Terminal UI and command-line client for the BitSafe Decentralization Manager",
    long_about = None,
    after_help = "Run without a command to open the terminal UI. With a command, decman-cli \
                  prints JSON on stdout, writes progress and errors on stderr, and exits non-zero \
                  on failure."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub connection: Connection,

    #[command(flatten)]
    pub options: CommandOptions,
}

/// How to reach and log in to one decman node when no profile is used.
#[derive(Debug, Args)]
pub struct Connection {
    /// Base URL of the decman API (for example `http://localhost:8081`).
    #[arg(long, env = "DECMAN_API_URL", global = true)]
    pub api_url: Option<String>,

    /// Frontend username to log in with.
    #[arg(long, env = "DECMAN_USERNAME", global = true)]
    pub username: Option<String>,

    /// Frontend password to log in with.
    #[arg(long, env = "DECMAN_PASSWORD", hide_env_values = true, global = true)]
    pub password: Option<String>,

    /// Shell command that prints the password, used instead of --password.
    #[arg(long, env = "DECMAN_PASSWORD_COMMAND", global = true)]
    pub password_command: Option<String>,

    /// OAuth2 token endpoint (auto-discovered from `/auth-config` if unset).
    #[arg(long, env = "OAUTH_TOKEN_URL", global = true)]
    pub oauth_token_url: Option<String>,

    /// OAuth2 client id (auto-discovered from `/auth-config` if unset).
    #[arg(long, env = "OAUTH_CLIENT_ID", global = true)]
    pub oauth_client_id: Option<String>,

    /// OAuth2 client secret, for confidential clients only.
    #[arg(
        long,
        env = "OAUTH_CLIENT_SECRET",
        hide_env_values = true,
        global = true
    )]
    pub oauth_client_secret: Option<String>,

    /// OAuth2 audience (required by some IdPs, e.g. Auth0).
    #[arg(long, env = "OAUTH_AUDIENCE", global = true)]
    pub oauth_audience: Option<String>,

    /// OAuth2 scope(s), space-separated.
    #[arg(long, env = "OAUTH_SCOPE", global = true)]
    pub oauth_scope: Option<String>,
}

impl Connection {
    /// Build the password-grant auth settings, treating empty optional env vars
    /// as unset.
    ///
    /// # Errors
    ///
    /// Returns an error if both a password and a password command are set.
    pub fn auth_settings(&self) -> Result<AuthSettings> {
        let non_empty = |value: &Option<String>| value.clone().filter(|s| !s.is_empty());

        Ok(AuthSettings {
            username: self.username.clone().unwrap_or_default(),
            password: password_source(
                "--password / --password-command",
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

    /// The API URL, required for a login without a profile.
    ///
    /// # Errors
    ///
    /// Returns an error if no API URL is set.
    pub fn require_api_url(&self) -> Result<String> {
        self.api_url
            .clone()
            .filter(|url| !url.is_empty())
            .context("no login selected: pass --profile, or set --api-url / DECMAN_API_URL")
    }
}

/// Options for the command mode. The terminal UI ignores them.
#[derive(Debug, Args)]
pub struct CommandOptions {
    /// Login profile from the config file.
    #[arg(short, long, env = "DECMAN_PROFILE", global = true)]
    pub profile: Option<String>,

    /// Config file with the login profiles. Default: `config.toml` in the
    /// current directory.
    #[arg(long, env = "DECMAN_CONFIG", global = true)]
    pub config: Option<PathBuf>,

    /// Allow commands that change state on a profile with `require_yes`.
    #[arg(long, global = true)]
    pub yes: bool,

    /// Print JSON on one line.
    #[arg(long, global = true)]
    pub compact: bool,

    /// Do not read or write the token cache.
    #[arg(long, global = true)]
    pub no_token_cache: bool,

    /// Seconds before one API request times out.
    #[arg(long, value_name = "SECONDS", default_value_t = 120, global = true)]
    pub http_timeout: u64,
}
