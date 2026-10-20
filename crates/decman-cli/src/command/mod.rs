//! Non-interactive commands: each one calls the decman API and prints JSON.
//!
//! The commands map to the API routes the web frontend uses. A route without
//! a command is still reachable through `decman-cli api`.

mod contracts;
mod governance;
mod input;
mod wait;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use self::governance::Target;
use self::wait::{RunSelector, WaitOptions, wait_for_run};
use crate::api::{ApiError, DecmanClient, mesh_error_hint};

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

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// HTTP method, for example GET or POST.
    method: String,
    /// Route path, for example /governance/state.
    path: String,
    /// Query parameter as key=value. Repeat for more.
    #[arg(short, long = "query", value_name = "KEY=VALUE")]
    queries: Vec<String>,
    /// JSON body: inline JSON, @file, or - for stdin.
    #[arg(long)]
    body: Option<String>,
}

/// `--wait` and its polling settings, shared by every command that starts a
/// workflow run.
#[derive(Debug, Args)]
pub struct WaitArgs {
    /// Wait until the workflow run ends. Exit non-zero unless it completes.
    #[arg(long)]
    wait: bool,
    /// Give up waiting after this many seconds.
    #[arg(long, value_name = "SECONDS", requires = "wait")]
    timeout: Option<u64>,
    /// Seconds between status polls.
    #[arg(long, value_name = "SECONDS", default_value_t = 2, requires = "wait")]
    poll_interval: u64,
}

impl WaitArgs {
    fn options(&self) -> WaitOptions {
        WaitOptions {
            timeout: self.timeout.map(Duration::from_secs),
            interval: Duration::from_secs(self.poll_interval.max(1)),
        }
    }
}

#[derive(Debug, Args)]
pub struct PartyArg {
    /// Decentralized party: its full id, or its name (the part before `::`)
    /// when only one party on this node has that name.
    #[arg(long)]
    party: String,
}

#[derive(Debug, Subcommand)]
pub enum NodeCommand {
    /// This node's participant id, name, and Noise endpoint.
    Config,
    /// Health of each hop from this node to Canton and its peers.
    Health,
    /// Status of this node's Noise key.
    Keys,
    /// The operator party of this node.
    Operator,
    /// The DSO party and amulet rules of the network.
    NetworkInfo,
}

#[derive(Debug, Subcommand)]
pub enum PeersCommand {
    /// Connectivity of each configured peer.
    Status,
    /// The configured peer list.
    Config,
    /// Replace the configured peer list.
    SetConfig {
        /// The peer list as `GET /network-config` returns it, or only its
        /// `peers` array: inline, @file, or -.
        #[arg(long)]
        body: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PartiesCommand {
    /// The decentralized parties this node is a member of.
    List {
        /// Only parties whose name starts with this prefix.
        #[arg(long)]
        prefix: Option<String>,
        /// Include each party's contracts. This read is slow.
        #[arg(long)]
        include_contracts: bool,
        /// Read from Canton now instead of the cache.
        #[arg(long)]
        refresh: bool,
    },
    /// The external parties this node hosts.
    External,
    /// Create a decentralized party with the given peers (onboarding).
    Create {
        /// Party id prefix, for example my-network.
        #[arg(long)]
        prefix: String,
        /// Participant id of a peer to invite. Repeat for more.
        #[arg(long = "peer", required = true)]
        peers: Vec<String>,
        /// Signing threshold. The server default is a majority.
        #[arg(long)]
        threshold: Option<i32>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Add a participant to a party.
    AddMember {
        #[command(flatten)]
        party: PartyArg,
        /// Participant id to add. It must be a configured peer.
        #[arg(long)]
        participant: String,
        /// Threshold after the change.
        #[arg(long)]
        threshold: i32,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Remove a participant from a party (kick).
    RemoveMember {
        #[command(flatten)]
        party: PartyArg,
        /// Participant id to remove.
        #[arg(long)]
        participant: String,
        /// Threshold after the change.
        #[arg(long)]
        threshold: i32,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Change a party's signing threshold.
    SetThreshold {
        #[command(flatten)]
        party: PartyArg,
        /// New threshold.
        #[arg(long)]
        threshold: i32,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum PackagesCommand {
    /// Packages vetted on this node.
    Vetted,
    /// Splice package versions the network expects now.
    Expected,
    /// Compare this node's vetted packages with peers.
    Compare {
        /// Participant id to compare with. Repeat for more. Default: every peer.
        #[arg(long = "participant")]
        participants: Vec<String>,
    },
    /// Package ids decman uses for a party, such as governance core.
    Config {
        #[command(flatten)]
        party: PartyArg,
    },
}

#[derive(Debug, Subcommand)]
pub enum DarsCommand {
    /// Upload DAR files to this node only.
    Upload {
        /// DAR files to upload.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Distribute DAR files to peers. The peers must approve the invitation.
    Distribute {
        /// DAR files to distribute.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Participant id of a peer. Repeat for more.
        #[arg(long = "peer", required = true)]
        peers: Vec<String>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Distribute a DAR this node already holds, named by one of its packages.
    DistributePackage {
        /// A package id in the DAR.
        #[arg(long)]
        package_id: String,
        /// Participant id of a peer. Repeat for more.
        #[arg(long = "peer", required = true)]
        peers: Vec<String>,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum InvitationsCommand {
    /// Pending invitations.
    List,
    /// Invitations already accepted or declined, newest first.
    Handled {
        #[arg(long)]
        limit: Option<i64>,
    },
    /// Accept an invitation. This starts this node's side of the workflow.
    Accept {
        /// Invitation id.
        id: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Decline an invitation.
    Decline {
        /// Invitation id.
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum WorkflowsCommand {
    /// Runs in progress, and ended runs nobody has dismissed.
    List,
    /// One run by name.
    Get {
        /// Run instance name.
        name: String,
    },
    /// Wait until a run ends. Exit non-zero unless it completes.
    Wait {
        /// Run instance name.
        name: String,
        /// Give up after this many seconds.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
        /// Seconds between status polls.
        #[arg(long, value_name = "SECONDS", default_value_t = 2)]
        poll_interval: u64,
    },
    /// Cancel a run in progress.
    Cancel {
        /// Run instance name.
        name: String,
    },
    /// Dismiss an ended run from the list.
    Dismiss {
        /// Run instance name.
        name: String,
    },
    /// Resume a failed run.
    Retry {
        /// Run instance name.
        name: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum ContractsCommand {
    /// Deploy contracts, for example the governance rules.
    Deploy {
        /// JSON body with decentralized_party_id, participant_ids,
        /// participant_parties, operator_party, and contracts: inline, @file,
        /// or -.
        #[arg(long)]
        body: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Deploy the governance rules (Governance Core) for a party. Each
    /// participant of the party needs a member party: the one its node
    /// reports, or the one --member gives.
    DeployGovernance {
        #[command(flatten)]
        party: PartyArg,
        /// The member party of one participant, as PARTICIPANT=PARTY. Repeat
        /// for more. It replaces the member party the peers report.
        #[arg(long = "member", value_name = "PARTICIPANT=PARTY")]
        members: Vec<String>,
        /// Confirmations an action needs before it can execute.
        #[arg(long)]
        threshold: i64,
        /// How long a confirmation stays valid, in microseconds. The web UI
        /// proposes 86400000000 (24 hours).
        #[arg(long, value_name = "MICROSECONDS")]
        confirmation_timeout_micros: i64,
        /// A party that may propose besides the members. Repeat for more.
        #[arg(long = "extra-proposer")]
        extra_proposers: Vec<String>,
        /// Operator party. Default: this node's operator party.
        #[arg(long)]
        operator_party: Option<String>,
        /// Print the request body and do not send it.
        #[arg(long, conflicts_with = "wait")]
        dry_run: bool,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum PartyConfigCommand {
    /// A party's credentials on this node. Secrets are masked.
    Get {
        #[command(flatten)]
        party: PartyArg,
    },
    /// Save a party's credentials. Give the body as @file or - so secrets
    /// stay off the command line.
    Set {
        /// JSON body: inline, @file, or -.
        #[arg(long)]
        body: String,
    },
    /// Find the member party and user id the given credentials map to.
    DiscoverMemberParty {
        /// JSON body: inline, @file, or -.
        #[arg(long)]
        body: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Canton auth status of each party's credentials.
    Status,
    /// Mint a token with each party's credentials to test them.
    Test,
    /// Grant the party user its actAs and readAs rights.
    GrantRights {
        #[command(flatten)]
        party: PartyArg,
        /// Client id of an IdP client with Canton admin rights. Its secret is
        /// read from `DECMAN_ADMIN_CLIENT_SECRET` only, so that it stays out
        /// of the shell history and the process list.
        #[arg(long)]
        admin_client_id: String,
    },
}

/// Names one pending governance action.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("target").required(true).args(["proposal", "confirmation", "action"])))]
pub struct TargetArgs {
    /// Contract id of an on-chain proposal.
    #[arg(long)]
    proposal: Option<String>,
    /// Contract id of any confirmation on the action.
    #[arg(long)]
    confirmation: Option<String>,
    /// Full action JSON of a self-governance action: inline, @file, or -.
    #[arg(long)]
    action: Option<String>,
}

impl TargetArgs {
    fn target(&self) -> Result<Target> {
        match (&self.proposal, &self.confirmation, &self.action) {
            (Some(cid), _, _) => Ok(Target::Proposal(cid.clone())),
            (_, Some(cid), _) => Ok(Target::Confirmation(cid.clone())),
            (_, _, Some(action)) => Ok(Target::Action(input::read_json(action)?)),
            _ => bail!("name the action with --proposal, --confirmation, or --action"),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum GovernanceCommand {
    /// The governance rules: members, threshold, and timeout.
    State {
        #[command(flatten)]
        party: PartyArg,
    },
    /// Pending actions and proposals with their confirmations.
    Pending {
        #[command(flatten)]
        party: PartyArg,
    },
    /// Proposals, one page at a time.
    Proposals {
        #[command(flatten)]
        party: PartyArg,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// The members the rules know, with their member parties.
    KnownMembers {
        #[command(flatten)]
        party: PartyArg,
    },
    /// This node's governance audit log.
    Audit {
        #[command(flatten)]
        party: PartyArg,
        #[arg(long)]
        limit: Option<i64>,
    },
    /// The governance history read from the ledger.
    ChainAudit {
        #[command(flatten)]
        party: PartyArg,
        #[arg(long)]
        limit: Option<usize>,
        /// Read from Canton now instead of the cache.
        #[arg(long)]
        refresh: bool,
    },
    /// Create a proposal. This node's confirmation is added to it.
    Propose {
        #[command(flatten)]
        party: PartyArg,
        /// The proposal JSON: inline, @file, or -.
        #[arg(long)]
        body: String,
    },
    /// Confirm a pending action, or start a self action with --action.
    Confirm {
        #[command(flatten)]
        party: PartyArg,
        #[command(flatten)]
        target: TargetArgs,
    },
    /// Execute an action that has enough confirmations.
    Execute {
        #[command(flatten)]
        party: PartyArg,
        #[command(flatten)]
        target: TargetArgs,
        /// Disclosed contracts as a JSON array of {contract_id, blob}: inline,
        /// @file, or -.
        #[arg(long)]
        disclosed: Option<String>,
    },
    /// Revoke this node's own confirmation on an action.
    Revoke {
        #[command(flatten)]
        party: PartyArg,
        #[command(flatten)]
        target: TargetArgs,
    },
    /// Archive a confirmation that has expired.
    Expire {
        #[command(flatten)]
        party: PartyArg,
        /// Contract id of the expired confirmation.
        #[arg(long)]
        confirmation: String,
    },
    /// Retract a proposal this node made.
    CancelProposal {
        #[command(flatten)]
        party: PartyArg,
        /// Contract id of the proposal.
        #[arg(long)]
        proposal: String,
    },
}

/// Ledger reads that take a party and return a page of contracts.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LedgerResource {
    ProviderServices,
    UserServices,
    RegistrarServices,
    CredentialOffers,
    Credentials,
    RegistrarServiceRequests,
    ProviderConfigurations,
    TransferInstructions,
    MintRequests,
    BurnRequests,
    TransferPreapprovals,
    Instruments,
    TransferFactories,
    Holdings,
    CouponReassignmentDelegation,
}

impl LedgerResource {
    fn path(self) -> &'static str {
        match self {
            Self::ProviderServices => "/services/provider",
            Self::UserServices => "/services/user",
            Self::RegistrarServices => "/services/registrar",
            Self::CredentialOffers => "/credential-offers",
            Self::Credentials => "/credentials",
            Self::RegistrarServiceRequests => "/registrar-service-requests",
            Self::ProviderConfigurations => "/provider-configurations",
            Self::TransferInstructions => "/governance/transfer-instructions",
            Self::MintRequests => "/governance/mint-requests",
            Self::BurnRequests => "/governance/burn-requests",
            Self::TransferPreapprovals => "/transfer-preapprovals",
            Self::Instruments => "/instruments",
            Self::TransferFactories => "/transfer-factories",
            Self::Holdings => "/holdings",
            Self::CouponReassignmentDelegation => "/coupon-reassignment-delegation",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum LedgerCommand {
    /// One kind of contract the party sees.
    List {
        resource: LedgerResource,
        #[command(flatten)]
        party: PartyArg,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Active contracts of one template or interface.
    Query {
        #[command(flatten)]
        party: PartyArg,
        #[arg(long)]
        package_id: String,
        #[arg(long)]
        module: String,
        #[arg(long)]
        entity: String,
        /// The template id names an interface.
        #[arg(long)]
        interface: bool,
    },
}

/// Run `command` against `client`.
///
/// # Errors
///
/// Returns an error if an input cannot be read, the API rejects a call, or a
/// wait fails.
pub fn run(client: &mut DecmanClient, command: Command) -> Result<Outcome> {
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
        Command::PartyConfig(command) => run_party_config(client, command),
        Command::Auth(command) => run_auth(client, command),
        Command::Governance(command) => run_governance(client, command),
        Command::Ledger(command) => run_ledger(client, command),
    }
}

/// Environment variable that holds the admin client secret for
/// `auth grant-rights`.
const ADMIN_SECRET_ENV: &str = "DECMAN_ADMIN_CLIENT_SECRET";

/// `POST /network-config` takes a bare peer array. Accept the
/// `{"peers": [...]}` object that `GET /network-config` returns too.
fn peer_list(body: Value) -> Value {
    match body {
        Value::Object(mut object) if object.contains_key("peers") => {
            object.remove("peers").unwrap_or(Value::Null)
        }
        other => other,
    }
}

/// `value` as one URL path segment. Characters outside the unreserved set
/// and `:` are percent-encoded, so a name cannot reach another route.
///
/// # Errors
///
/// Returns an error for an empty value, `.`, or `..`.
fn segment(value: &str) -> Result<String> {
    if value.is_empty() || value == "." || value == ".." {
        bail!("{value:?} is not a valid name");
    }
    Ok(value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect())
}

fn get(client: &mut DecmanClient, path: &str, query: &[(&str, String)]) -> Result<Outcome> {
    let query: Vec<(String, String)> = query
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    Ok(Outcome::ok(client.request(
        Method::GET,
        path,
        &query,
        None,
    )?))
}

fn post(client: &mut DecmanClient, path: &str, body: Option<&Value>) -> Result<Outcome> {
    Ok(Outcome::ok(client.request(
        Method::POST,
        path,
        &[],
        body,
    )?))
}

/// Push optional query parameters onto `query`.
fn optional(query: &mut Vec<(&'static str, String)>, key: &'static str, value: Option<String>) {
    if let Some(value) = value {
        query.push((key, value));
    }
}

fn run_api(client: &mut DecmanClient, args: &ApiArgs) -> Result<Outcome> {
    let method = Method::from_bytes(args.method.to_ascii_uppercase().as_bytes())
        .with_context(|| format!("{:?} is not an HTTP method", args.method))?;
    let path = if args.path.starts_with('/') {
        args.path.clone()
    } else {
        format!("/{}", args.path)
    };
    let query = args
        .queries
        .iter()
        .map(|pair| input::query_pair(pair))
        .collect::<Result<Vec<_>>>()?;
    let body = args.body.as_deref().map(input::read_json).transpose()?;
    Ok(Outcome::ok(client.request(
        method,
        &path,
        &query,
        body.as_ref(),
    )?))
}

fn run_node(client: &mut DecmanClient, command: &NodeCommand) -> Result<Outcome> {
    let path = match command {
        NodeCommand::Config => "/node-config",
        NodeCommand::Health => "/node-health",
        NodeCommand::Keys => "/keys/status",
        NodeCommand::Operator => "/operator-info",
        NodeCommand::NetworkInfo => "/network-info",
    };
    get(client, path, &[])
}

fn run_peers(client: &mut DecmanClient, command: PeersCommand) -> Result<Outcome> {
    match command {
        PeersCommand::Status => get(client, "/participants-status", &[]),
        PeersCommand::Config => get(client, "/network-config", &[]),
        PeersCommand::SetConfig { body } => {
            let peers = peer_list(input::read_json(&body)?);
            post(client, "/network-config", Some(&peers))
        }
    }
}

/// A party resolved from a [`PartyArg`], with its current threshold when the
/// node lists it.
struct ResolvedParty {
    id: String,
    threshold: Option<i64>,
    /// The participant ids that host the party. Empty when the node does not
    /// list the party.
    participants: Vec<String>,
}

/// Resolve a party name or id against `GET /decentralized-parties`. A full id
/// that the list does not hold is passed through, so a command can still name
/// a party this node does not cache.
fn resolve_party(client: &mut DecmanClient, party: &PartyArg) -> Result<ResolvedParty> {
    let arg = party.party.as_str();
    let name = arg.split("::").next().unwrap_or(arg);
    let response = client.request(
        Method::GET,
        "/decentralized-parties",
        &[("prefix".to_owned(), name.to_owned())],
        None,
    )?;
    let parties = response
        .get("parties")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let matches = matching_parties(&parties, arg);
    match matches.as_slice() {
        [party] => Ok(ResolvedParty {
            id: party["party_id"].as_str().unwrap_or(arg).to_owned(),
            threshold: party["threshold"].as_i64(),
            participants: party["participants"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|participant| participant["participant_uid"].as_str())
                .map(str::to_owned)
                .collect(),
        }),
        [] if arg.contains("::") => Ok(ResolvedParty {
            id: arg.to_owned(),
            threshold: None,
            participants: Vec::new(),
        }),
        [] => bail!("this node has no decentralized party named {arg}"),
        several => {
            let ids: Vec<&str> = several
                .iter()
                .filter_map(|party| party["party_id"].as_str())
                .collect();
            bail!(
                "several parties are named {arg}; pass a full party id: {}",
                ids.join(", ")
            )
        }
    }
}

/// The parties `arg` names: the one with that full id, or every party whose
/// name (the part before `::`) equals `arg`.
fn matching_parties<'a>(parties: &'a [Value], arg: &str) -> Vec<&'a Value> {
    parties
        .iter()
        .filter(|party| {
            let id = party["party_id"].as_str().unwrap_or_default();
            if arg.contains("::") {
                id == arg
            } else {
                id.split("::").next() == Some(arg)
            }
        })
        .collect()
}

/// Start a workflow with `POST path` and, with `--wait`, wait for its run.
fn start_workflow(
    client: &mut DecmanClient,
    path: &str,
    body: &Value,
    wait: &WaitArgs,
) -> Result<Outcome> {
    let response = client.request(Method::POST, path, &[], Some(body))?;
    if !wait.wait {
        return Ok(Outcome::ok(response));
    }
    let name = response
        .get("instance_name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .context("the response names no workflow run to wait for")?
        .to_owned();
    eprintln!("started workflow run {name}");
    wait_for_run(client, &RunSelector::Instance(name), wait.options())
}

fn run_parties(client: &mut DecmanClient, command: PartiesCommand) -> Result<Outcome> {
    match command {
        PartiesCommand::List {
            prefix,
            include_contracts,
            refresh,
        } => {
            let mut query = Vec::new();
            optional(&mut query, "prefix", prefix);
            if include_contracts {
                query.push(("include_contracts", "true".to_owned()));
            }
            if refresh {
                query.push(("refresh", "true".to_owned()));
            }
            get(client, "/decentralized-parties", &query)
        }
        PartiesCommand::External => get(client, "/external-parties", &[]),
        PartiesCommand::Create {
            prefix,
            peers,
            threshold,
            wait,
        } => {
            let body =
                json!({ "party_id_prefix": prefix, "peer_ids": peers, "threshold": threshold });
            start_workflow(client, "/onboarding", &body, &wait).map_err(with_mesh_hint)
        }
        PartiesCommand::AddMember {
            party,
            participant,
            threshold,
            wait,
        } => {
            let party = resolve_party(client, &party)?;
            let body = json!({
                "decentralized_party_id": party.id,
                "new_participant_id": participant,
                "new_threshold": threshold,
                "previous_threshold": party.threshold.unwrap_or(0),
            });
            start_workflow(client, "/add-party", &body, &wait)
        }
        PartiesCommand::RemoveMember {
            party,
            participant,
            threshold,
            wait,
        } => {
            let party = resolve_party(client, &party)?;
            let body = json!({
                "decentralized_party_id": party.id,
                "participant_id": participant,
                "new_threshold": threshold,
                "previous_threshold": party.threshold.unwrap_or(0),
            });
            start_workflow(client, "/kick", &body, &wait)
        }
        PartiesCommand::SetThreshold {
            party,
            threshold,
            wait,
        } => {
            let party = resolve_party(client, &party)?;
            let body = json!({
                "decentralized_party_id": party.id,
                "new_threshold": threshold,
                "previous_threshold": party.threshold.unwrap_or(0),
            });
            start_workflow(client, "/change-threshold", &body, &wait)
        }
    }
}

/// Replace a `422` onboarding error with its mesh remediation hints.
fn with_mesh_hint(error: anyhow::Error) -> anyhow::Error {
    let hint = error
        .downcast_ref::<ApiError>()
        .filter(|api| api.status == StatusCode::UNPROCESSABLE_ENTITY)
        .and_then(|api| mesh_error_hint(&api.body));
    match hint {
        Some(hint) => anyhow::anyhow!("{hint}"),
        None => error,
    }
}

fn run_packages(client: &mut DecmanClient, command: PackagesCommand) -> Result<Outcome> {
    match command {
        PackagesCommand::Vetted => get(client, "/packages/vetted", &[]),
        PackagesCommand::Expected => get(client, "/packages/expected-versions", &[]),
        PackagesCommand::Compare { participants } => {
            let mut query = Vec::new();
            if !participants.is_empty() {
                query.push(("participants", participants.join(",")));
            }
            get(client, "/packages/compare-peers", &query)
        }
        PackagesCommand::Config { party } => {
            let party = resolve_party(client, &party)?;
            get(client, "/packages", &[("party_id", party.id)])
        }
    }
}

fn run_dars(client: &mut DecmanClient, command: DarsCommand) -> Result<Outcome> {
    match command {
        DarsCommand::Upload { files } => {
            let body = json!({ "dar_files": input::dar_files(&files)?, "peer_ids": [] });
            post(client, "/dars/upload", Some(&body))
        }
        DarsCommand::Distribute { files, peers, wait } => {
            let body = json!({ "dar_files": input::dar_files(&files)?, "peer_ids": peers });
            start_workflow(client, "/dars/distribute", &body, &wait)
        }
        DarsCommand::DistributePackage {
            package_id,
            peers,
            wait,
        } => {
            let body = json!({ "package_id": package_id, "peer_ids": peers });
            start_workflow(client, "/dars/distribute-package", &body, &wait)
        }
    }
}

fn run_invitations(client: &mut DecmanClient, command: InvitationsCommand) -> Result<Outcome> {
    match command {
        InvitationsCommand::List => get(client, "/invitations", &[]),
        InvitationsCommand::Handled { limit } => {
            let mut query = Vec::new();
            optional(&mut query, "limit", limit.map(|limit| limit.to_string()));
            get(client, "/invitations/handled", &query)
        }
        InvitationsCommand::Accept { id, wait } => {
            // Read the invitation first: after the accept it is gone, and the
            // wait needs its coordinator run to find this node's peer run.
            let selector = if wait.wait {
                Some(peer_run_selector(client, &id)?)
            } else {
                None
            };
            let response = client.request(
                Method::POST,
                "/invitations/accept",
                &[],
                Some(&json!({ "id": id })),
            )?;
            match selector {
                Some(selector) => wait_for_run(client, &selector, wait.options()),
                None => Ok(Outcome::ok(response)),
            }
        }
        InvitationsCommand::Decline { id } => {
            post(client, "/invitations/decline", Some(&json!({ "id": id })))
        }
    }
}

/// The selector for the peer run that accepting invitation `id` starts.
fn peer_run_selector(client: &mut DecmanClient, id: &str) -> Result<RunSelector> {
    let response = client.request(Method::GET, "/invitations", &[], None)?;
    let invitation = response
        .get("invitations")
        .and_then(Value::as_array)
        .and_then(|invitations| invitations.iter().find(|invitation| invitation["id"] == id))
        .with_context(|| format!("no pending invitation has id {id}"))?;
    let field = |key: &str| {
        invitation[key]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .with_context(|| format!("invitation {id} has no {key}, so --wait cannot find its run"))
    };
    Ok(RunSelector::PeerOf {
        coordinator_instance: field("workflow_instance")?,
        coordinator_pubkey: field("coordinator_pubkey")?,
    })
}

fn run_workflows(client: &mut DecmanClient, command: WorkflowsCommand) -> Result<Outcome> {
    match command {
        WorkflowsCommand::List => get(client, "/workflows", &[]),
        WorkflowsCommand::Get { name } => {
            let response = client.request(Method::GET, "/workflows", &[], None)?;
            let run = response
                .get("runs")
                .and_then(Value::as_array)
                .and_then(|runs| {
                    runs.iter()
                        .find(|run| run["instance_name"] == name.as_str())
                })
                .with_context(|| format!("workflow run {name} is not in /workflows"))?;
            Ok(Outcome::ok(run.clone()))
        }
        WorkflowsCommand::Wait {
            name,
            timeout,
            poll_interval,
        } => {
            let options = WaitOptions {
                timeout: timeout.map(Duration::from_secs),
                interval: Duration::from_secs(poll_interval.max(1)),
            };
            wait_for_run(client, &RunSelector::Instance(name), options)
        }
        WorkflowsCommand::Cancel { name } => post(
            client,
            &format!("/workflows/{}/cancel", segment(&name)?),
            None,
        ),
        WorkflowsCommand::Dismiss { name } => post(
            client,
            &format!("/workflows/{}/dismiss", segment(&name)?),
            None,
        ),
        WorkflowsCommand::Retry { name, wait } => {
            let response = client.request(
                Method::POST,
                &format!("/workflows/{}/retry", segment(&name)?),
                &[],
                None,
            )?;
            if wait.wait {
                wait_for_run(client, &RunSelector::Instance(name), wait.options())
            } else {
                Ok(Outcome::ok(response))
            }
        }
    }
}

fn run_contracts(client: &mut DecmanClient, command: ContractsCommand) -> Result<Outcome> {
    match command {
        ContractsCommand::Deploy { body, wait } => {
            start_workflow(client, "/contracts", &input::read_json(&body)?, &wait)
        }
        ContractsCommand::DeployGovernance {
            party,
            threshold,
            confirmation_timeout_micros,
            members,
            extra_proposers,
            operator_party,
            dry_run,
            wait,
        } => {
            let overrides = members
                .iter()
                .map(|member| input::query_pair(member))
                .collect::<Result<Vec<_>>>()?;
            let party = resolve_party(client, &party)?;
            if party.participants.is_empty() {
                bail!("this node does not list {} with its participants", party.id);
            }
            let context = contracts::fetch_context(
                client,
                &party.id,
                &party.participants,
                &overrides,
                operator_party,
            )?;
            let input = contracts::GovernanceCoreInput {
                threshold,
                confirmation_timeout_micros,
                extra_proposers,
            };
            let body = contracts::body(&party.id, &context, &input)?;
            if dry_run {
                return Ok(Outcome::ok(body));
            }
            start_workflow(client, "/contracts", &body, &wait)
        }
    }
}

fn run_party_config(client: &mut DecmanClient, command: PartyConfigCommand) -> Result<Outcome> {
    match command {
        PartyConfigCommand::Get { party } => {
            let party = resolve_party(client, &party)?;
            get(
                client,
                &format!("/party-config/{}", segment(&party.id)?),
                &[],
            )
        }
        PartyConfigCommand::Set { body } => Ok(Outcome::ok(client.request(
            Method::PUT,
            "/party-config",
            &[],
            Some(&input::read_json(&body)?),
        )?)),
        PartyConfigCommand::DiscoverMemberParty { body } => post(
            client,
            "/party-config/discover-member-party",
            Some(&input::read_json(&body)?),
        ),
    }
}

fn run_auth(client: &mut DecmanClient, command: AuthCommand) -> Result<Outcome> {
    match command {
        AuthCommand::Status => get(client, "/auth/status", &[]),
        AuthCommand::Test => post(client, "/auth/test", None),
        AuthCommand::GrantRights {
            party,
            admin_client_id,
        } => {
            let admin_client_secret = std::env::var(ADMIN_SECRET_ENV)
                .ok()
                .filter(|secret| !secret.is_empty())
                .with_context(|| format!("set {ADMIN_SECRET_ENV} to the admin client secret"))?;
            let party = resolve_party(client, &party)?;
            let body = json!({
                "dec_party_id": party.id,
                "admin_client_id": admin_client_id,
                "admin_client_secret": admin_client_secret,
            });
            post(client, "/auth/grant-rights", Some(&body))
        }
    }
}

fn run_governance(client: &mut DecmanClient, command: GovernanceCommand) -> Result<Outcome> {
    match command {
        GovernanceCommand::State { party } => {
            let party = resolve_party(client, &party)?;
            get(client, "/governance/state", &[("party_id", party.id)])
        }
        GovernanceCommand::Pending { party } => {
            let party = resolve_party(client, &party)?;
            get(
                client,
                "/governance/confirmations",
                &[("party_id", party.id)],
            )
        }
        GovernanceCommand::Proposals {
            party,
            limit,
            cursor,
        } => {
            let party = resolve_party(client, &party)?;
            let mut query = vec![("party_id", party.id)];
            optional(&mut query, "limit", limit.map(|limit| limit.to_string()));
            optional(&mut query, "cursor", cursor);
            get(client, "/governance/proposals", &query)
        }
        GovernanceCommand::KnownMembers { party } => {
            let party = resolve_party(client, &party)?;
            get(
                client,
                "/governance/known-members",
                &[("party_id", party.id)],
            )
        }
        GovernanceCommand::Audit { party, limit } => {
            let party = resolve_party(client, &party)?;
            let mut query = vec![("party_id", party.id)];
            optional(&mut query, "limit", limit.map(|limit| limit.to_string()));
            get(client, "/governance/audit", &query)
        }
        GovernanceCommand::ChainAudit {
            party,
            limit,
            refresh,
        } => {
            let party = resolve_party(client, &party)?;
            let mut query = vec![("party_id", party.id)];
            optional(&mut query, "limit", limit.map(|limit| limit.to_string()));
            if refresh {
                query.push(("refresh", "true".to_owned()));
            }
            get(client, "/governance/chain-audit", &query)
        }
        GovernanceCommand::Propose { party, body } => {
            let proposal = input::read_json(&body)?;
            let party = resolve_party(client, &party)?;
            let (_, rules_contract_id) = governance::fetch(client, &party.id)?;
            let body = json!({
                "party_id": party.id,
                "rules_contract_id": rules_contract_id,
                "proposal": proposal,
            });
            post(client, "/governance/propose", Some(&body))
        }
        GovernanceCommand::Confirm { party, target } => {
            let target = target.target()?;
            let party = resolve_party(client, &party)?;
            let (pending, rules) = governance::fetch(client, &party.id)?;
            let body = governance::confirm_body(&pending, &party.id, &rules, &target)?;
            post(client, "/governance/confirm", Some(&body))
        }
        GovernanceCommand::Execute {
            party,
            target,
            disclosed,
        } => {
            let target = target.target()?;
            let disclosed = match disclosed {
                Some(disclosed) => input::read_json(&disclosed)?,
                None => json!([]),
            };
            let party = resolve_party(client, &party)?;
            let (pending, rules) = governance::fetch(client, &party.id)?;
            let body = governance::execute_body(&pending, &party.id, &rules, &target, disclosed)?;
            post(client, "/governance/execute", Some(&body))
        }
        GovernanceCommand::Revoke { party, target } => {
            let target = target.target()?;
            let party = resolve_party(client, &party)?;
            let (pending, _) = governance::fetch(client, &party.id)?;
            let body = governance::revoke_body(&pending, &party.id, &target)?;
            post(client, "/governance/cancel", Some(&body))
        }
        GovernanceCommand::Expire {
            party,
            confirmation,
        } => {
            let party = resolve_party(client, &party)?;
            let (pending, rules) = governance::fetch(client, &party.id)?;
            let body = governance::expire_body(&pending, &party.id, &rules, &confirmation)?;
            post(client, "/governance/expire", Some(&body))
        }
        GovernanceCommand::CancelProposal { party, proposal } => {
            let party = resolve_party(client, &party)?;
            let (pending, _) = governance::fetch(client, &party.id)?;
            let body = governance::cancel_proposal_body(&pending, &party.id, &proposal)?;
            post(client, "/governance/cancel-proposal", Some(&body))
        }
    }
}

fn run_ledger(client: &mut DecmanClient, command: LedgerCommand) -> Result<Outcome> {
    match command {
        LedgerCommand::List {
            resource,
            party,
            limit,
            cursor,
        } => {
            let party = resolve_party(client, &party)?;
            let mut query = vec![("party_id", party.id)];
            optional(&mut query, "limit", limit.map(|limit| limit.to_string()));
            optional(&mut query, "cursor", cursor);
            get(client, resource.path(), &query)
        }
        LedgerCommand::Query {
            party,
            package_id,
            module,
            entity,
            interface,
        } => {
            let party = resolve_party(client, &party)?;
            let query = vec![
                ("party_id", party.id),
                ("package_id", package_id),
                ("module_name", module),
                ("entity_name", entity),
                ("interface", interface.to_string()),
            ];
            get(client, "/contracts/query", &query)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parties() -> Vec<Value> {
        vec![
            json!({ "party_id": "alpha::1220aa", "threshold": 2 }),
            json!({ "party_id": "alpha-v2::1220bb", "threshold": 1 }),
            json!({ "party_id": "gamma::1220cc", "threshold": 1 }),
            json!({ "party_id": "gamma::1220dd", "threshold": 1 }),
        ]
    }

    #[test]
    fn party_name_matches_the_whole_name_not_a_prefix() {
        // Arrange
        let parties = parties();

        // Act
        let matches = matching_parties(&parties, "alpha");

        // Assert
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["party_id"], "alpha::1220aa");
    }

    #[test]
    fn full_party_id_matches_exactly() {
        let parties = parties();
        let matches = matching_parties(&parties, "gamma::1220dd");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["party_id"], "gamma::1220dd");
    }

    #[test]
    fn ambiguous_party_name_matches_every_party_with_it() {
        assert_eq!(matching_parties(&parties(), "gamma").len(), 2);
    }

    #[test]
    fn mesh_hint_replaces_a_422_onboarding_error() {
        // Arrange
        let error: anyhow::Error = ApiError {
            method: Method::POST,
            path: "/onboarding".to_owned(),
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: r#"{"error":"Peers are not meshed.","missing_edges":[]}"#.to_owned(),
        }
        .into();

        // Act
        let message = with_mesh_hint(error).to_string();

        // Assert
        assert!(message.contains("Peers are not meshed."), "{message}");
        assert!(!message.contains("returned"), "{message}");
    }

    #[test]
    fn segment_keeps_party_ids_and_encodes_separators() {
        assert_eq!(segment("alpha::1220aa").unwrap(), "alpha::1220aa");
        assert_eq!(segment("../kick").unwrap(), "..%2Fkick");
        assert_eq!(segment("a b?c#d").unwrap(), "a%20b%3Fc%23d");
        assert!(segment("..").is_err());
        assert!(segment("").is_err());
    }

    #[test]
    fn peer_list_unwraps_the_get_response() {
        assert_eq!(
            peer_list(json!({ "peers": [{ "name": "a" }] })),
            json!([{ "name": "a" }])
        );
        assert_eq!(peer_list(json!([1])), json!([1]));
    }

    #[test]
    fn other_errors_keep_their_message() {
        let error = with_mesh_hint(anyhow::anyhow!("boom"));
        assert_eq!(error.to_string(), "boom");
    }
}
