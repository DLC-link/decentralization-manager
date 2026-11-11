//! Non-interactive commands: each one calls the decman API and prints JSON.
//!
//! The commands map to the API routes the web frontend uses. A route without
//! a command is still reachable through `decman-cli api`.

mod args;
mod contracts;
mod credentials;
mod governance;
mod http;
mod input;
mod node;
mod parties;
mod wait;

use anyhow::{Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use self::args::{ApiArgs, AuthCommand, ContractsCommand, DarsCommand, InvitationsCommand};
use self::args::{LedgerCommand, NodeCommand, PackagesCommand, PartiesCommand};
use self::args::{PartyConfigCommand, PeersCommand, WorkflowsCommand};
use self::governance::{GovernanceCommand, run_governance};
use self::node::{run_api, run_auth, run_contracts, run_dars, run_invitations, run_ledger};
use self::node::{run_node, run_packages, run_party_config, run_peers, run_workflows};
use self::parties::run_parties;
use crate::api::DecmanClient;
pub use crate::api::for_terminal;
use crate::config::{AdminClient, PartyCredentials};

/// The result of a command: the JSON to print, and whether the command
/// reached its goal. A workflow that ends failed prints its run and still
/// exits non-zero.
#[derive(Debug)]
pub struct Outcome {
    pub value: Value,
    pub success: bool,
}

impl Outcome {
    fn ok(value: Value) -> Self {
        Self {
            value,
            success: true,
        }
    }
}

/// Per-node settings a profile configures once, so that `auth grant-rights`
/// and `party-config` need no credentials on the command line. Secrets are
/// shell commands; the CLI runs one only when a command needs that secret.
#[derive(Clone, Debug, Default)]
pub struct NodeSettings {
    pub admin_client: Option<AdminClient>,
    pub party_credentials: Option<PartyCredentials>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the login profiles in the config file. Passwords are never shown.
    Profiles,
    /// Log in, or reuse the cached login, and print the token expiry.
    Login,
    /// Delete the cached tokens for this login.
    Logout,
    /// Send any request to the decman API, for routes without a command.
    Api(ApiArgs),
    /// Read this node's configuration and health.
    #[command(subcommand)]
    Node(NodeCommand),
    /// Read and set the peers this node talks to.
    #[command(subcommand)]
    Peers(PeersCommand),
    /// List decentralized parties and change their members or threshold.
    #[command(subcommand)]
    Parties(PartiesCommand),
    /// Read vetted packages and compare them with peers.
    #[command(subcommand)]
    Packages(PackagesCommand),
    /// Upload DARs to this node or distribute them to peers.
    #[command(subcommand)]
    Dars(DarsCommand),
    /// List, accept, and decline invitations from other nodes.
    #[command(subcommand)]
    Invitations(InvitationsCommand),
    /// List, wait for, cancel, dismiss, and retry workflow runs.
    #[command(subcommand)]
    Workflows(WorkflowsCommand),
    /// Deploy contracts for a decentralized party.
    #[command(subcommand)]
    Contracts(ContractsCommand),
    /// Read and set a party's ledger credentials on this node.
    #[command(subcommand)]
    PartyConfig(PartyConfigCommand),
    /// Check and grant the Canton rights of the party credentials.
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Propose, confirm, and execute governance actions.
    #[command(subcommand)]
    Governance(GovernanceCommand),
    /// Read a party's ledger contracts.
    #[command(subcommand)]
    Ledger(LedgerCommand),
}

/// Run `command` against `client`.
///
/// # Errors
///
/// Returns an error if an input cannot be read, the API rejects a call, or a
/// wait fails.
pub fn run(
    client: &mut DecmanClient,
    command: Command,
    settings: &NodeSettings,
) -> Result<Outcome> {
    match command {
        Command::Profiles => bail!("profiles needs no login; main lists them before login"),
        Command::Login => {
            let expires_at = client.login()?;
            Ok(Outcome::ok(json!({
                "api_url": client.base_url(),
                "access_token_expires_at": expires_at,
            })))
        }
        Command::Logout => {
            let removed = client.logout()?;
            Ok(Outcome::ok(json!({ "removed_cached_login": removed })))
        }
        Command::Api(args) => run_api(client, &args),
        Command::Node(command) => run_node(client, &command),
        Command::Peers(command) => run_peers(client, command),
        Command::Parties(command) => run_parties(client, command),
        Command::Packages(command) => run_packages(client, command),
        Command::Dars(command) => run_dars(client, command),
        Command::Invitations(command) => run_invitations(client, command),
        Command::Workflows(command) => run_workflows(client, command),
        Command::Contracts(command) => run_contracts(client, command),
        Command::PartyConfig(command) => run_party_config(client, command, settings),
        Command::Auth(command) => run_auth(client, command, settings),
        Command::Governance(command) => run_governance(client, command),
        Command::Ledger(command) => run_ledger(client, command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_text_loses_control_characters() {
        assert_eq!(for_terminal("a\u{1b}[31mb\nc\td"), "a?[31mb\nc\td");
    }
}
