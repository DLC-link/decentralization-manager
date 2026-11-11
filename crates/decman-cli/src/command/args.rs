//! The clap arguments and subcommands of every command group except
//! governance.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};

use super::wait::WaitOptions;

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// HTTP method, for example GET or POST.
    pub method: String,
    /// Route path, for example /governance/state.
    pub path: String,
    /// Query parameter as key=value. Repeat for more.
    #[arg(short, long = "query", value_name = "KEY=VALUE")]
    pub queries: Vec<String>,
    /// JSON body: inline JSON, @file, or - for stdin.
    #[arg(long)]
    pub body: Option<String>,
}

/// `--wait` and its polling settings, shared by every command that starts a
/// workflow run.
#[derive(Debug, Args)]
pub struct WaitArgs {
    /// Wait until the workflow run ends. Exit non-zero unless it completes.
    #[arg(long)]
    pub wait: bool,
    /// Give up waiting after this many seconds.
    #[arg(long, value_name = "SECONDS", requires = "wait")]
    timeout: Option<u64>,
    /// Seconds between status polls.
    #[arg(long, value_name = "SECONDS", default_value_t = 2, requires = "wait")]
    poll_interval: u64,
}

impl WaitArgs {
    pub fn options(&self) -> WaitOptions {
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
    pub party: String,
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
        /// How many hosting participants must confirm a transaction. The
        /// server default is the signing threshold.
        #[arg(long)]
        confirmation_threshold: Option<i32>,
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
        /// New confirmation threshold. By default a party keeps a confirmation
        /// threshold that differs from its signing threshold; otherwise the
        /// two stay equal.
        #[arg(long)]
        confirmation_threshold: Option<i32>,
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
    /// Download one DAR of a DAR invitation, to compare it before you accept.
    Dar {
        /// Invitation id.
        id: String,
        /// Position of the DAR in the invitation, from 0.
        index: usize,
        /// File to write the DAR to.
        #[arg(long)]
        out: PathBuf,
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
        /// A party that sees the governance contracts without being a member.
        /// Repeat for more.
        #[arg(long = "disclosure-observer")]
        disclosure_observers: Vec<String>,
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
    /// Save a party's credentials: the profile's [party_credentials] for
    /// --party, or a --body. A body without a client secret gets the one the
    /// profile's client_secret_command prints.
    Set {
        /// The party to configure with the profile's credentials.
        #[arg(long, required_unless_present = "body", conflicts_with = "body")]
        party: Option<String>,
        /// A JSON body instead of the profile's credentials: inline, @file,
        /// or -.
        #[arg(long)]
        body: Option<String>,
        /// Send no client secret, so the node keeps the one it stores.
        #[arg(long)]
        keep_secret: bool,
    },
    /// Find the member party and user id that credentials map to: the
    /// profile's [party_credentials], or a --body.
    DiscoverMemberParty {
        /// A JSON body instead of the profile's credentials: inline, @file,
        /// or -.
        #[arg(long)]
        body: Option<String>,
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
        /// Client id of an IdP client with Canton admin rights. Default: the
        /// profile's [admin_client] client_id. Its secret comes from
        /// `DECMAN_ADMIN_CLIENT_SECRET`, else from that section's
        /// client_secret_command, never from the command line.
        #[arg(long)]
        admin_client_id: Option<String>,
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
    pub fn path(self) -> &'static str {
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
