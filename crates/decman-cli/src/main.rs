mod api;
mod app;
mod auth;
mod cli;
#[cfg(test)]
mod client_tests;
mod command;
mod composer;
mod config;
mod logo;
mod ui;

use std::io::stdout;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use serde_json::{Value, json};
use tracing_subscriber::{EnvFilter, fmt};

use crate::api::{AuthSettings, DecmanClient};
use crate::app::{App, Outcome, run_login, spawn_workers};
use crate::auth::{PasswordSource, TokenCache};
use crate::cli::{Cli, CommandOptions, Connection};
use crate::command::{Command, NodeSettings};
use crate::config::Profile;

/// Entry point for decman-cli.
///
/// Without a command it opens the terminal UI. With a command it runs that
/// command, prints its JSON result on stdout, and exits non-zero on failure.
fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let Cli {
        command,
        connection,
        options,
    } = Cli::parse();
    let config_path = options
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(config::CONFIG_FILE));

    match command {
        Some(command) => run_command(command, &connection, &options, &config_path),
        None => match run_tui(&connection, &config_path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Error: {error:?}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Run one command and print its result. Errors go to stderr.
fn run_command(
    command: Command,
    connection: &Connection,
    options: &CommandOptions,
    config_path: &Path,
) -> ExitCode {
    match command_outcome(command, connection, options, config_path) {
        Ok(outcome) => {
            let printed = if options.compact {
                serde_json::to_string(&outcome.value)
            } else {
                serde_json::to_string_pretty(&outcome.value)
            };
            match printed {
                Ok(text) => println!("{text}"),
                Err(error) => {
                    eprintln!("error: failed to print the result: {error}");
                    return ExitCode::FAILURE;
                }
            }
            if outcome.success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("error: {}", command::for_terminal(&format!("{error:#}")));
            ExitCode::FAILURE
        }
    }
}

fn command_outcome(
    command: Command,
    connection: &Connection,
    options: &CommandOptions,
    config_path: &Path,
) -> Result<command::Outcome> {
    if matches!(command, Command::Profiles) {
        return list_profiles(config_path);
    }
    let login = select_login(connection, options, config_path)?;
    let mut client = DecmanClient::new(login.api_url.clone(), login.auth.clone())?
        .with_request_timeout(Duration::from_secs(options.http_timeout))
        .with_warnings();
    if !options.no_token_cache
        && let Some(cache) = TokenCache::for_login(&login.api_url, &login.auth.username)
    {
        client = client.with_token_cache(cache);
    }
    if login.require_yes && !options.yes {
        client = client.with_writes_blocked(format!(
            "profile {} needs --yes for every request that is not a read (GET)",
            login.name
        ));
    }
    command::run(&mut client, command, &login.settings)
}

/// The login a command runs with.
struct Login {
    name: String,
    api_url: String,
    auth: AuthSettings,
    require_yes: bool,
    settings: NodeSettings,
}

impl Login {
    fn from_profile(profile: &Profile) -> Result<Self> {
        Ok(Self {
            name: profile.name.clone(),
            api_url: profile.api_url.clone(),
            auth: profile.auth()?,
            require_yes: profile.require_yes,
            settings: NodeSettings {
                admin_client: profile.admin_client.clone(),
                party_credentials: profile.party_credentials.clone(),
            },
        })
    }
}

/// Pick the login: `--profile` first, then `--api-url` / `DECMAN_API_URL`,
/// then the only profile in the config file.
fn select_login(
    connection: &Connection,
    options: &CommandOptions,
    config_path: &Path,
) -> Result<Login> {
    let profiles = config::load_profiles(config_path)?;
    let names = || {
        profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(name) = &options.profile {
        let profile = profiles
            .iter()
            .find(|profile| &profile.name == name)
            .with_context(|| {
                format!(
                    "no profile named {name} in {path} (profiles: {names})",
                    path = config_path.display(),
                    names = names(),
                )
            })?;
        return Login::from_profile(profile);
    }
    if connection.api_url.is_some() {
        let api_url = connection.require_api_url()?;
        // A protected node stays protected when it is reached by URL.
        let require_yes = profiles
            .iter()
            .any(|profile| profile.require_yes && same_node(&profile.api_url, &api_url));
        return Ok(Login {
            name: api_url.clone(),
            api_url,
            auth: connection.auth_settings()?,
            require_yes,
            settings: NodeSettings::default(),
        });
    }
    match profiles.as_slice() {
        [profile] => Login::from_profile(profile),
        [] => bail!(
            "no login selected: pass --profile, or set --api-url / DECMAN_API_URL ({} has no profiles)",
            config_path.display()
        ),
        _ => bail!(
            "{path} has several profiles; pick one with --profile ({names})",
            path = config_path.display(),
            names = names(),
        ),
    }
}

/// Whether two API URLs name the same node: same scheme, host and port
/// (`https://a` and `https://A:443/` match), ignoring a trailing `/`.
fn same_node(left: &str, right: &str) -> bool {
    let key = |url: &str| {
        reqwest::Url::parse(url).ok().map(|url| {
            (
                url.scheme().to_owned(),
                url.host_str().map(str::to_ascii_lowercase),
                url.port_or_known_default(),
                url.path().trim_end_matches('/').to_owned(),
            )
        })
    };
    match (key(left), key(right)) {
        (Some(left), Some(right)) => left == right,
        _ => left.trim_end_matches('/') == right.trim_end_matches('/'),
    }
}

/// The profiles in the config file, without their passwords.
fn list_profiles(config_path: &Path) -> Result<command::Outcome> {
    let profiles = config::load_profiles(config_path)?;
    let mut listed = Vec::new();
    for profile in &profiles {
        let password = match profile.auth()?.password {
            Some(PasswordSource::Literal(_)) => "password",
            Some(PasswordSource::Command(_)) => "password_command",
            None => "none",
        };
        listed.push(json!({
            "name": profile.name,
            "network": profile.network,
            "api_url": profile.api_url,
            "username": profile.username,
            "password_from": password,
            "require_yes": profile.require_yes,
        }));
    }
    Ok(command::Outcome {
        value: Value::Array(listed),
        success: true,
    })
}

/// Run the terminal UI: config file profiles if there are any, else the
/// single login from the flags and `.env`.
fn run_tui(connection: &Connection, config_path: &Path) -> Result<()> {
    init_tracing();
    tracing::info!("Starting decman-cli terminal UI");

    let profiles = config::load_profiles(config_path)?;

    // Resolve the `.env` login *before* taking over the terminal, so a missing
    // setting exits cleanly instead of leaving the terminal in raw mode.
    // The password command runs now, while the terminal is still normal, so
    // that a password manager prompt does not draw over the UI.
    let env_session = if profiles.is_empty() {
        let api_url = connection
            .api_url
            .clone()
            .filter(|url| !url.is_empty())
            .with_context(|| {
                format!(
                    "no login: add profiles to {}, or set --api-url / DECMAN_API_URL",
                    config_path.display()
                )
            })?;
        let auth = connection.auth_settings()?;
        if auth.username.is_empty() || auth.password.is_none() {
            bail!("set --username and --password (or --password-command), or their env vars");
        }
        Some((api_url, resolve_password_now(auth)?))
    } else {
        None
    };

    let mut terminal = ratatui::init();
    enable_mouse();
    // `ratatui::init` installs a panic hook that restores the terminal; chain it
    // so a panic also turns mouse capture back off (else the terminal is left
    // emitting mouse escape codes).
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        disable_mouse();
        previous_hook(info);
    }));

    let result = match env_session {
        Some((api_url, auth)) => run_env(&mut terminal, api_url, auth),
        None => run_profiles(&mut terminal, profiles),
    };
    disable_mouse();
    ratatui::restore();

    tracing::info!("decman-cli terminal UI exited");
    result
}

/// Turn on mouse capture so the TUI receives scroll / click events. Best-effort:
/// a terminal that rejects it just leaves the keyboard-only experience intact.
fn enable_mouse() {
    let _ = execute!(stdout(), EnableMouseCapture);
}

/// Turn mouse capture back off, restoring the terminal's native text selection.
fn disable_mouse() {
    let _ = execute!(stdout(), DisableMouseCapture);
}

/// Run the app against the single profile configured in `.env`.
fn run_env(terminal: &mut DefaultTerminal, api_url: String, auth: AuthSettings) -> Result<()> {
    // No logging past this point: stderr writes corrupt the alternate screen.
    let (requests, updates) = spawn_workers(api_url, auth, app::PEER_POLL_INTERVAL);
    App::new(requests, updates, false).run(terminal)?;
    Ok(())
}

/// Run the login / session loop over `config.toml` profiles: pick a profile
/// (or auto-login a remembered one), run the app, and on logout return to the
/// menu.
fn run_profiles(terminal: &mut DefaultTerminal, profiles: Vec<Profile>) -> Result<()> {
    loop {
        let profile = match remembered(&profiles) {
            Some(profile) => profile,
            None => match run_login(terminal, &profiles)? {
                Some(profile) => {
                    config::remember_profile(&profile.name);
                    profile
                }
                None => return Ok(()),
            },
        };

        // No logging here: stderr writes would corrupt the alternate screen.
        let auth = profile.auth()?;
        let auth = if matches!(auth.password, Some(PasswordSource::Command(_))) {
            // Leave the UI while the password command runs, so that a password
            // manager prompt gets a normal terminal.
            disable_mouse();
            ratatui::restore();
            let resolved = resolve_password_now(auth);
            *terminal = ratatui::init();
            enable_mouse();
            resolved?
        } else {
            auth
        };
        let (requests, updates) =
            spawn_workers(profile.api_url.clone(), auth, app::PEER_POLL_INTERVAL);
        match App::new(requests, updates, true).run(terminal)? {
            Outcome::Quit => return Ok(()),
            Outcome::Logout => config::forget_profile(),
        }
    }
}

/// `auth` with a password command replaced by the password it prints. The
/// TUI's workers then never run the command while the UI owns the terminal.
fn resolve_password_now(mut auth: AuthSettings) -> Result<AuthSettings> {
    if let Some(source @ PasswordSource::Command(_)) = &auth.password {
        auth.password = Some(PasswordSource::Literal(source.resolve()?));
    }
    Ok(auth)
}

/// The remembered profile from a previous session, if it still exists.
fn remembered(profiles: &[Profile]) -> Option<Profile> {
    let name = config::remembered_profile()?;
    profiles
        .iter()
        .find(|profile| profile.name == name)
        .cloned()
}

/// Initialize tracing, writing to stderr so the alternate screen used by the
/// TUI is never corrupted. Honors `RUST_LOG`, defaulting to `info`.
fn init_tracing() {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let mut argv = vec!["decman-cli"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).unwrap()
    }

    fn config(dir: &tempfile::TempDir, toml: &str) -> PathBuf {
        let path = dir.path().join("config.toml");
        std::fs::write(&path, toml).unwrap();
        path
    }

    const PROTECTED: &str = r#"
        [[profiles]]
        name = "guarded"
        api_url = "https://decman.example"
        username = "alice"
        password = "hunter2"
        require_yes = true

        [[profiles]]
        name = "open"
        api_url = "https://other.example"
        username = "bob"
        password_command = "pass show bob"
    "#;

    #[test]
    fn the_cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn same_node_ignores_case_default_port_and_trailing_slash() {
        assert!(same_node(
            "https://decman.example",
            "https://DECMAN.example:443/"
        ));
        assert!(!same_node(
            "https://decman.example",
            "https://decman.example:8443"
        ));
        assert!(!same_node(
            "https://decman.example",
            "http://decman.example"
        ));
    }

    #[test]
    fn a_url_login_to_a_protected_node_keeps_the_guard() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let path = config(&dir, PROTECTED);
        let cli = parse(&[
            "--api-url",
            "https://DECMAN.example/",
            "--username",
            "x",
            "profiles",
        ]);

        // Act
        let login = select_login(&cli.connection, &cli.options, &path).unwrap();

        // Assert
        assert!(login.require_yes);
    }

    #[test]
    fn a_profile_wins_over_a_url() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let path = config(&dir, PROTECTED);
        let cli = parse(&[
            "-p",
            "open",
            "--api-url",
            "https://decman.example",
            "profiles",
        ]);

        // Act
        let login = select_login(&cli.connection, &cli.options, &path).unwrap();

        // Assert
        assert_eq!(login.name, "open");
        assert!(!login.require_yes);
    }

    #[test]
    fn several_profiles_without_a_choice_is_an_error() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let path = config(&dir, PROTECTED);
        let cli = parse(&["profiles"]);

        // Act
        let error = select_login(&cli.connection, &cli.options, &path)
            .err()
            .unwrap();

        // Assert
        assert!(
            error.to_string().contains("pick one with --profile"),
            "{error}"
        );
    }

    #[test]
    fn listing_profiles_never_shows_a_password() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let path = config(&dir, PROTECTED);

        // Act
        let outcome = list_profiles(&path).unwrap();

        // Assert
        let text = outcome.value.to_string();
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains("pass show bob"), "{text}");
        assert!(text.contains("password_command"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn the_tui_resolves_a_password_command_before_it_starts() {
        // Arrange
        let auth = AuthSettings {
            username: "alice".to_owned(),
            password: Some(PasswordSource::Command("printf 's3cret\\n'".to_owned())),
            token_url: None,
            client_id: None,
            client_secret: None,
            audience: None,
            scope: None,
        };

        // Act
        let resolved = resolve_password_now(auth).unwrap();

        // Assert
        assert!(matches!(resolved.password, Some(PasswordSource::Literal(ref p)) if p == "s3cret"));
    }
}
