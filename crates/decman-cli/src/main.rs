mod api;
mod app;
mod cli;
mod composer;
mod config;
mod logo;
mod ui;

use anyhow::Result;
use clap::Parser;
use ratatui::DefaultTerminal;
use tracing_subscriber::{EnvFilter, fmt};

use crate::api::AuthSettings;
use crate::app::{App, Outcome, run_login, spawn_workers};
use crate::cli::Cli;
use crate::config::Profile;

/// Entry point for the decman-cli terminal UI.
///
/// Loads configuration (`config.toml` profiles, else `.env`), initializes the
/// terminal, runs the login / app loop, and restores the terminal on exit.
///
/// # Errors
///
/// Returns an error if config cannot be parsed, or the terminal cannot be
/// drawn to / read from.
fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();
    tracing::info!("Starting decman-cli terminal UI");

    let profiles = config::load_profiles()?;

    // Parse the `.env` profile *before* taking over the terminal, so a missing
    // required arg exits cleanly via clap instead of leaving the terminal in
    // raw mode.
    let env_session = if profiles.is_empty() {
        let cli = Cli::parse();
        Some((cli.api_url.clone(), cli.auth_settings()))
    } else {
        None
    };

    let mut terminal = ratatui::init();
    let result = match env_session {
        Some((api_url, auth)) => run_env(&mut terminal, api_url, auth),
        None => run_profiles(&mut terminal, profiles),
    };
    ratatui::restore();

    tracing::info!("decman-cli terminal UI exited");
    result
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
        let (requests, updates) = spawn_workers(
            profile.api_url.clone(),
            profile.auth(),
            app::PEER_POLL_INTERVAL,
        );
        match App::new(requests, updates, true).run(terminal)? {
            Outcome::Quit => return Ok(()),
            Outcome::Logout => config::forget_profile(),
        }
    }
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
