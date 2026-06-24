use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use base64::prelude::*;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;
use serde_json::{Value, json};

use common::types::{
    AuditLogEntry, DecentralizedParty, PeerPackageComparison, VettedPackageInfo, WorkflowKind,
    WorkflowProgress, WorkflowRole,
};

use crate::api::{
    AuthSettings, ChainAuditEntry, DecmanClient, DomainGovAction, ExecuteParams, FeedItem,
    GovAction, GovConfirmation, GovState, GovernanceConfirmations, Holding, KnownMember,
    PartyAuthStatus, PeerEntry, PeerView, party_name,
};
use crate::composer::{
    self, Composer, ComposerContext, ComposerSubmit, FieldKind, SelectOption, TypeOption,
};
use crate::config::Profile;
use crate::ui;

/// Per-party detail extras (holdings + audit), each fetched independently so
/// one failing does not hide the other.
pub struct DetailData {
    pub holdings: Result<Vec<Holding>, String>,
    pub audit: Result<Vec<AuditLogEntry>, String>,
    pub gov_state: Result<Option<GovState>, String>,
}

/// How the main app exited: quit the program, or log out back to the menu.
pub enum Outcome {
    Quit,
    Logout,
}

/// Drive the login menu until the user picks a profile or quits.
///
/// # Errors
///
/// Returns an error if drawing a frame or reading an input event fails.
pub fn run_login(terminal: &mut DefaultTerminal, profiles: &[Profile]) -> Result<Option<Profile>> {
    // Wipe any prior screen (e.g. the app on logout) before showing the menu.
    terminal.clear()?;

    let mut state = TableState::default();
    state.select(Some(0));

    loop {
        terminal.draw(|frame| ui::draw_login(frame, profiles, &mut state))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('q') | KeyCode::Esc, _) => return Ok(None),
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(None),
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                let next = state.selected().map_or(0, |i| (i + 1) % profiles.len());
                state.select(Some(next));
            }
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                let len = profiles.len();
                let previous = state.selected().map_or(0, |i| (i + len - 1) % len);
                state.select(Some(previous));
            }
            (KeyCode::Enter, _) => return Ok(profiles.get(state.selected().unwrap_or(0)).cloned()),
            _ => {}
        }
    }
}

/// How often the background thread re-probes peers (matches the web frontend).
pub const PEER_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Longest the input loop blocks before redrawing, so background updates
/// (loaded data, live peer latency, spinner) appear without a keypress.
const TICK: Duration = Duration::from_millis(200);

/// A request to the background fetcher.
pub enum Request {
    Parties,
    Dars,
    Feed,
    Compare,
    Accept(String),
    Decline(String),
    Dismiss(String),
    Upload {
        filename: String,
        data: String,
    },
    Distribute {
        filename: String,
        data: String,
        peer_ids: Vec<String>,
    },
    Detail(String),
    Onboard {
        prefix: String,
        peer_ids: Vec<String>,
    },
    Kick {
        party_id: String,
        participant_id: String,
        new_threshold: i32,
        previous_threshold: i32,
    },
    CancelWorkflow(WorkflowKind),
    RetryWorkflow(String),
    ChainAudit(String),
    AuthStatus,
    TestAuth,
    GrantRights {
        dec_party_id: String,
        client_id: String,
        client_secret: String,
    },
    Governance {
        party_id: String,
        party_name: String,
        governance_type: String,
    },
    GovAction(Box<GovActionRequest>),
    OperatorInfo,
    DeployContext {
        party_id: String,
        party_name: String,
    },
    DeploySubmit(Box<Value>),
    NetworkPeers,
    SaveNetwork(Box<Value>),
}

/// A result delivered by a background worker.
pub enum Update {
    Parties(Result<Vec<DecentralizedParty>, String>),
    Dars(Result<Vec<VettedPackageInfo>, String>),
    Feed(Result<Vec<FeedItem>, String>),
    Peers(Result<Vec<PeerView>, String>),
    Compare(Result<PeerPackageComparison, String>),
    Action(Result<String, String>),
    Detail(DetailData),
    ChainAudit(Result<Vec<ChainAuditEntry>, String>),
    AuthStatus(Result<Vec<PartyAuthStatus>, String>),
    Governance(Result<Box<GovView>, String>),
    OperatorInfo(String),
    DeployContext(Result<Box<DeployForm>, String>),
    NetworkPeers(Result<Vec<PeerEntry>, String>),
}

/// A DAR file picked from disk for upload / distribution.
pub struct Dar {
    pub filename: String,
    pub data: String,
}

/// A peer toggled in the distribution dialog.
pub struct PeerChoice {
    pub id: String,
    pub name: String,
    pub checked: bool,
}

/// The onboarding (create-party) form: a party-id prefix plus a tickable list
/// of peers to invite. `cursor` 0 selects the prefix field; `1..=peers.len()`
/// select a peer row.
pub struct OnboardForm {
    pub prefix: String,
    pub peers: Vec<PeerChoice>,
    pub cursor: usize,
}

impl OnboardForm {
    /// Whether the prefix field (cursor 0) is focused.
    fn on_prefix(&self) -> bool {
        self.cursor == 0
    }

    /// The peer row index under the cursor, if a peer row is focused.
    fn peer_index(&self) -> Option<usize> {
        self.cursor.checked_sub(1)
    }
}

/// A participant that may be kicked from a party (an owner of the party).
pub struct KickCandidate {
    pub participant_id: String,
    pub label: String,
}

/// The kick-participant form: pick an owner to remove and set the new
/// signing threshold for the remaining owners.
pub struct KickForm {
    pub party_id: String,
    pub party_name: String,
    pub previous_threshold: i32,
    pub candidates: Vec<KickCandidate>,
    pub selected: usize,
    pub new_threshold: i32,
    /// Highest threshold allowed after the kick (remaining owner count).
    pub max_threshold: i32,
}

/// One pending governance item shown in the approvals overlay: an off-chain
/// action or an on-chain (core-domain) proposal.
pub enum GovItem {
    OffChain(GovAction),
    Domain(DomainGovAction),
}

/// State of the governance-approvals overlay for one party.
pub struct GovView {
    pub party_name: String,
    pub party_id: String,
    /// The party's governance type (`core_self` or `vault`), used for off-chain
    /// actions and to refresh the overlay after a mutation.
    pub governance_type: String,
    pub rules_contract_id: String,
    pub member_party_id: String,
    pub threshold: i32,
    pub items: Vec<GovItem>,
    pub selected: usize,
}

/// A governance mutation to perform against a pending action.
enum GovOp {
    Confirm {
        action: Value,
        governance_type: String,
        proposal_cid: Option<String>,
    },
    Execute {
        action: Value,
        confirmation_cids: Vec<String>,
        governance_type: String,
        proposal_cid: Option<String>,
    },
    Cancel {
        confirmation_cid: String,
        governance_type: String,
    },
    Expire {
        confirmation_cid: String,
        governance_type: String,
    },
    /// Propose a new on-chain action (`POST /governance/propose`).
    Propose { proposal: Value },
}

/// Which composer the type picker is choosing for.
#[derive(Clone, Copy)]
pub enum ComposerKind {
    /// A brand-new off-chain governance action (submitted via confirm).
    Action,
    /// A new on-chain governance proposal.
    Proposal,
}

/// The action / proposal type picker, before a specific form is opened.
pub struct ComposerPick {
    pub kind: ComposerKind,
    pub party_id: String,
    pub party_name: String,
    pub governance_type: String,
    pub rules_contract_id: String,
    pub default_threshold: i64,
    pub options: Vec<TypeOption>,
    pub selected: usize,
}

/// The governance-core contract deployment form. The member set, participant
/// ids and package id are resolved from the server; the operator only edits the
/// initial governance threshold and the proposal timeout (the contract's field
/// structure is fixed, mirroring the web frontend's locked gov-core preset).
pub struct DeployForm {
    pub party_id: String,
    pub party_name: String,
    /// The `governance_core` package id (empty if the DAR is not vetted).
    pub package_id: String,
    pub operator_party: String,
    /// Participant uids, aligned with `member_parties`.
    pub participant_ids: Vec<String>,
    /// Member party ids (the governance member set).
    pub member_parties: Vec<String>,
    pub threshold: String,
    pub timeout_micros: String,
    /// 0 = threshold, 1 = timeout, 2 = submit row.
    pub cursor: usize,
}

/// A governance mutation plus the context needed to refresh the overlay after.
pub struct GovActionRequest {
    op: GovOp,
    party_id: String,
    party_name: String,
    /// Party-level governance type, used to refresh the overlay after the op.
    governance_type: String,
    rules_contract_id: String,
}

/// The add-peer sub-form of the network-config editor.
pub struct PeerForm {
    pub participant_id: String,
    pub name: String,
    pub address: String,
    pub port: String,
    pub public_key: String,
    pub party: String,
    /// 0..=5 select a field; 6 is the submit row.
    pub cursor: usize,
}

impl PeerForm {
    fn blank() -> Self {
        Self {
            participant_id: String::new(),
            name: String::new(),
            address: String::new(),
            port: String::new(),
            public_key: String::new(),
            party: String::new(),
            cursor: 0,
        }
    }
}

/// State of the network-config editor: the editable peer list, plus an
/// optional in-progress add-peer form.
pub struct NetworkEditState {
    pub peers: Vec<PeerEntry>,
    pub selected: usize,
    pub adding: Option<PeerForm>,
}

/// The grant-rights form: an admin client id + secret used to grant act/read
/// rights for one party. The secret is sent once and not stored.
pub struct GrantForm {
    pub dec_party_id: String,
    pub party_name: String,
    pub client_id: String,
    pub client_secret: String,
    /// 0 = client id, 1 = secret, 2 = submit row.
    pub cursor: usize,
}

/// A modal overlay drawn above the main UI.
pub enum Overlay {
    None,
    /// A blocking action is in flight.
    Busy(String),
    /// A result or error message awaiting dismissal.
    Message(String),
    /// The package checker results.
    Compare {
        comparison: PeerPackageComparison,
        scroll: u16,
    },
    /// A tickable peer list for DAR distribution.
    PeerSelect {
        dar: Dar,
        peers: Vec<PeerChoice>,
        cursor: usize,
    },
    /// Syntax-highlighted JSON for an expanded audit entry.
    Json {
        value: Value,
        scroll: u16,
    },
    /// The onboarding (create-party) form.
    Onboard(OnboardForm),
    /// The kick-participant form.
    Kick(KickForm),
    /// A scrollable detail view of a feed item (workflow run or invitation).
    /// Boxed: a feed item (with its workflow run) is large relative to the
    /// other overlay variants.
    FeedDetail {
        item: Box<FeedItem>,
        scroll: u16,
    },
    /// The on-chain governance audit trail for the open party.
    ChainAudit {
        entries: Vec<ChainAuditEntry>,
        scroll: u16,
    },
    /// Per-party authentication status and rights.
    Auth {
        parties: Vec<PartyAuthStatus>,
        selected: usize,
    },
    /// The grant-rights form for a selected party.
    GrantRights(Box<GrantForm>),
    /// The governance approvals list for a party (confirm / execute / revoke).
    Governance(Box<GovView>),
    /// The action / proposal type picker.
    ComposerPick(Box<ComposerPick>),
    /// A composer form for a chosen action / proposal variant.
    Composer(Box<Composer>),
    /// The governance-core contract deployment form.
    Deploy(Box<DeployForm>),
    /// The network-config (peer list) editor.
    NetworkEdit(Box<NetworkEditState>),
}

/// The selectable top-level views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Parties,
    Peers,
    Dars,
    Workflows,
}

impl Tab {
    /// All tabs, in display order.
    pub const ALL: [Tab; 4] = [Tab::Parties, Tab::Peers, Tab::Dars, Tab::Workflows];

    /// Title shown in the tab bar.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Parties => "Parties",
            Tab::Peers => "Peers",
            Tab::Dars => "Dars",
            Tab::Workflows => "Workflows",
        }
    }

    /// The next tab, wrapping around.
    fn next(self) -> Tab {
        let all = Tab::ALL;
        let index = all.iter().position(|t| *t == self).unwrap_or(0);
        all[(index + 1) % all.len()]
    }

    /// The previous tab, wrapping around.
    fn previous(self) -> Tab {
        let all = Tab::ALL;
        let index = all.iter().position(|t| *t == self).unwrap_or(0);
        all[(index + all.len() - 1) % all.len()]
    }

    /// Whether this tab's table can be searched.
    fn searchable(self) -> bool {
        matches!(self, Tab::Parties | Tab::Dars)
    }

    /// The on-demand fetch request for this tab, if any (peers auto-refresh).
    fn request(self) -> Option<Request> {
        match self {
            Tab::Parties => Some(Request::Parties),
            Tab::Dars => Some(Request::Dars),
            Tab::Workflows => Some(Request::Feed),
            Tab::Peers => None,
        }
    }
}

/// State of a tab's data fetch, driving what the UI renders.
#[derive(Debug)]
pub enum Status {
    Loading,
    Loaded,
    Error(String),
}

/// Borrowed render data for the active tab, handed to the UI layer. Parties and
/// Dars are pre-filtered by their search query.
pub enum TabView<'a> {
    Parties(&'a Status, Vec<&'a DecentralizedParty>, &'a mut TableState),
    Peers(&'a Status, &'a [PeerView], &'a mut TableState),
    Dars(&'a Status, Vec<&'a VettedPackageInfo>, &'a mut TableState),
    Workflows(&'a Status, &'a [FeedItem], &'a mut TableState),
}

/// Case-insensitive substring match of `query` against any of `fields`. An
/// empty query matches everything.
fn query_matches(query: &str, fields: &[&str]) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    fields
        .iter()
        .any(|field| field.to_lowercase().contains(&needle))
}

/// Parties matching `query` (by name or party id).
fn filter_parties<'a>(
    parties: &'a [DecentralizedParty],
    query: &str,
) -> Vec<&'a DecentralizedParty> {
    parties
        .iter()
        .filter(|party| {
            let party_id = party.party_id.to_string();
            query_matches(query, &[party_name(party), &party_id])
        })
        .collect()
}

/// Packages matching `query` (by name or package id).
fn filter_dars<'a>(dars: &'a [VettedPackageInfo], query: &str) -> Vec<&'a VettedPackageInfo> {
    dars.iter()
        .filter(|package| query_matches(query, &[&package.package_name, &package.package_id]))
        .collect()
}

/// Validate a decentralized-party id prefix, mirroring the web frontend's
/// onboarding rules: starts with a letter, only `[A-Za-z0-9_-]`, ≤180 chars.
fn validate_prefix(prefix: &str) -> Result<(), String> {
    match prefix.chars().next() {
        None => return Err("Party id prefix is required.".to_owned()),
        Some(c) if !c.is_ascii_alphabetic() => {
            return Err("Party id prefix must start with a letter.".to_owned());
        }
        Some(_) => {}
    }
    if prefix.chars().count() > 180 {
        return Err("Party id prefix must be 180 characters or fewer.".to_owned());
    }
    if !prefix
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Party id prefix may only contain letters, digits, '-' and '_'.".to_owned());
    }
    Ok(())
}

/// Keep a table's selection in range after its data changes.
fn clamp_selection(table: &mut TableState, len: usize) {
    let selected = match table.selected() {
        Some(index) if index < len => Some(index),
        _ => (len > 0).then_some(0),
    };
    table.select(selected);
}

/// Spawn the background workers. One thread services on-demand requests; a
/// second re-probes peers every `peer_interval`. Both stream results over the
/// shared `Update` channel and exit when the app quits.
pub fn spawn_workers(
    base_url: String,
    auth: AuthSettings,
    peer_interval: Duration,
) -> (Sender<Request>, Receiver<Update>) {
    let (request_tx, request_rx) = mpsc::channel::<Request>();
    let (update_tx, update_rx) = mpsc::channel::<Update>();

    let peer_tx = update_tx.clone();
    let peer_url = base_url.clone();
    let peer_auth = auth.clone();
    thread::spawn(move || {
        let mut client = match DecmanClient::new(peer_url, peer_auth) {
            Ok(client) => client,
            Err(error) => {
                let _ = peer_tx.send(Update::Peers(Err(format!("{error:#}"))));
                return;
            }
        };
        loop {
            // Re-probe peers and refresh the workflows feed on the same cadence
            // the web frontend uses, so live progress and incoming invitations
            // appear without the operator pressing refresh.
            let peers = client.fetch_peers().map_err(|error| format!("{error:#}"));
            if peer_tx.send(Update::Peers(peers)).is_err() {
                break;
            }
            let feed = client.fetch_feed().map_err(|error| format!("{error:#}"));
            if peer_tx.send(Update::Feed(feed)).is_err() {
                break;
            }
            thread::sleep(peer_interval);
        }
    });

    thread::spawn(move || {
        let mut client = match DecmanClient::new(base_url, auth) {
            Ok(client) => client,
            Err(error) => {
                let _ = update_tx.send(Update::Parties(Err(format!("{error:#}"))));
                return;
            }
        };
        while let Ok(request) = request_rx.recv() {
            let update = handle_request(&mut client, request);
            if update_tx.send(update).is_err() {
                break;
            }
        }
    });

    (request_tx, update_rx)
}

/// Run a single request against the client, mapping errors to display strings.
fn handle_request(client: &mut DecmanClient, request: Request) -> Update {
    let err = |error: anyhow::Error| format!("{error:#}");
    match request {
        Request::Parties => Update::Parties(client.fetch_parties().map_err(err)),
        Request::Dars => Update::Dars(client.fetch_dars().map_err(err)),
        Request::Feed => Update::Feed(client.fetch_feed().map_err(err)),
        Request::Compare => Update::Compare(client.compare_packages().map_err(err)),
        Request::Accept(id) => Update::Action(
            client
                .accept_invitation(&id)
                .map(|()| "Invitation accepted".to_owned())
                .map_err(err),
        ),
        Request::Decline(id) => Update::Action(
            client
                .decline_invitation(&id)
                .map(|()| "Invitation declined".to_owned())
                .map_err(err),
        ),
        Request::Dismiss(name) => Update::Action(
            client
                .dismiss_workflow(&name)
                .map(|()| "Workflow dismissed".to_owned())
                .map_err(err),
        ),
        Request::Upload { filename, data } => Update::Action(
            client
                .upload_dar(&filename, &data)
                .map(|()| format!("Uploaded {filename}"))
                .map_err(err),
        ),
        Request::Distribute {
            filename,
            data,
            peer_ids,
        } => Update::Action(
            client
                .distribute_dar(&filename, &data, &peer_ids)
                .map(|()| "Distribution started".to_owned())
                .map_err(err),
        ),
        Request::Detail(party_id) => Update::Detail(DetailData {
            holdings: client.fetch_holdings(&party_id).map_err(err),
            audit: client.fetch_audit(&party_id).map_err(err),
            gov_state: client.fetch_governance_state(&party_id).map_err(err),
        }),
        Request::ChainAudit(party_id) => {
            Update::ChainAudit(client.fetch_chain_audit(&party_id).map_err(err))
        }
        Request::AuthStatus => Update::AuthStatus(client.fetch_auth_status().map_err(err)),
        Request::TestAuth => Update::AuthStatus(
            client
                .test_auth()
                .and_then(|()| client.fetch_auth_status())
                .map_err(err),
        ),
        Request::GrantRights {
            dec_party_id,
            client_id,
            client_secret,
        } => Update::AuthStatus(
            client
                .grant_rights(&dec_party_id, &client_id, &client_secret)
                .and_then(|()| client.fetch_auth_status())
                .map_err(err),
        ),
        Request::Governance {
            party_id,
            party_name,
            governance_type,
        } => Update::Governance(
            build_gov_view(client, &party_id, &party_name, &governance_type)
                .map(Box::new)
                .map_err(err),
        ),
        Request::GovAction(request) => {
            let GovActionRequest {
                op,
                party_id,
                party_name,
                governance_type,
                rules_contract_id,
            } = *request;
            let result = perform_gov_op(client, &party_id, &rules_contract_id, &op)
                .and_then(|()| build_gov_view(client, &party_id, &party_name, &governance_type))
                .map(Box::new)
                .map_err(err);
            Update::Governance(result)
        }
        Request::OperatorInfo => match client.fetch_operator_info() {
            Ok(party_id) => Update::OperatorInfo(party_id),
            // Operator info is best-effort prefill; swallow errors silently.
            Err(_) => Update::OperatorInfo(String::new()),
        },
        Request::DeployContext {
            party_id,
            party_name,
        } => Update::DeployContext(
            build_deploy_context(client, &party_id, &party_name)
                .map(Box::new)
                .map_err(err),
        ),
        Request::DeploySubmit(body) => Update::Action(
            client
                .deploy_contracts(*body)
                .map(|()| "Contracts workflow started".to_owned())
                .map_err(err),
        ),
        Request::NetworkPeers => Update::NetworkPeers(client.fetch_network_peers().map_err(err)),
        Request::SaveNetwork(body) => Update::Action(
            client
                .save_network_peers(*body)
                .map(|()| "Network configuration saved".to_owned())
                .map_err(err),
        ),
        Request::Onboard { prefix, peer_ids } => Update::Action(
            client
                .start_onboarding(&prefix, &peer_ids)
                .map(|()| format!("Onboarding started for {prefix}"))
                .map_err(err),
        ),
        Request::Kick {
            party_id,
            participant_id,
            new_threshold,
            previous_threshold,
        } => Update::Action(
            client
                .kick_participant(
                    &party_id,
                    &participant_id,
                    new_threshold,
                    previous_threshold,
                )
                .map(|()| "Kick started".to_owned())
                .map_err(err),
        ),
        Request::CancelWorkflow(kind) => Update::Action(
            client
                .cancel_workflow(kind)
                .map(|()| "Workflow cancelled".to_owned())
                .map_err(err),
        ),
        Request::RetryWorkflow(name) => Update::Action(
            client
                .retry_workflow(&name)
                .map(|()| "Workflow retry started".to_owned())
                .map_err(err),
        ),
    }
}

/// Fetch the pending governance confirmations for a party and assemble the
/// approvals view (off-chain actions first, then on-chain proposals).
fn build_gov_view(
    client: &mut DecmanClient,
    party_id: &str,
    party_name: &str,
    governance_type: &str,
) -> Result<GovView> {
    let GovernanceConfirmations {
        actions,
        domain_actions,
        threshold,
        rules_contract_id,
        member_party_id,
    } = client.fetch_governance(party_id)?;
    // Confirm/execute/propose all need a real rules contract id; bail early with
    // a clear message rather than sending empty ids that fail obscurely later.
    let rules_contract_id = rules_contract_id
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("no governance rules contract found for this party"))?;
    let items = actions
        .into_iter()
        .map(GovItem::OffChain)
        .chain(domain_actions.into_iter().map(GovItem::Domain))
        .collect();
    Ok(GovView {
        party_name: party_name.to_owned(),
        party_id: party_id.to_owned(),
        governance_type: governance_type.to_owned(),
        rules_contract_id,
        // Empty is tolerated: it only weakens "revoke", which already handles
        // not finding this node's own confirmation gracefully.
        member_party_id: member_party_id.unwrap_or_default(),
        threshold,
        items,
        selected: 0,
    })
}

/// Perform one governance mutation against the API.
fn perform_gov_op(
    client: &mut DecmanClient,
    party_id: &str,
    rules_contract_id: &str,
    op: &GovOp,
) -> Result<()> {
    match op {
        GovOp::Confirm {
            action,
            governance_type,
            proposal_cid,
        } => client.confirm_action(
            party_id,
            rules_contract_id,
            action,
            governance_type,
            proposal_cid.as_deref(),
        ),
        GovOp::Execute {
            action,
            confirmation_cids,
            governance_type,
            proposal_cid,
        } => client.execute_action(
            party_id,
            rules_contract_id,
            &ExecuteParams {
                action: action.clone(),
                confirmation_cids: confirmation_cids.clone(),
                governance_type: governance_type.clone(),
                proposal_cid: proposal_cid.clone(),
                disclosed: Vec::new(),
            },
        ),
        GovOp::Cancel {
            confirmation_cid,
            governance_type,
        } => client.cancel_confirmation(party_id, confirmation_cid, governance_type),
        GovOp::Expire {
            confirmation_cid,
            governance_type,
        } => client.expire_confirmation(
            party_id,
            rules_contract_id,
            confirmation_cid,
            governance_type,
        ),
        GovOp::Propose { proposal } => client.propose_action(party_id, rules_contract_id, proposal),
    }
}

/// The contract ids of a set of confirmations, for the execute call.
fn confirmation_cids(confirmations: &[GovConfirmation]) -> Vec<String> {
    confirmations
        .iter()
        .map(|confirmation| confirmation.contract_id.clone())
        .collect()
}

/// Validate the add-peer form into a [`PeerEntry`], or return the index of the
/// first invalid field so the cursor can land on it. Required: participant id
/// (0), address (2), a non-zero `u16` port (3) and public key (4); name (1) and
/// party (5) are optional.
fn validate_peer_form(form: &PeerForm) -> Result<PeerEntry, usize> {
    if form.participant_id.trim().is_empty() {
        return Err(0);
    }
    if form.address.trim().is_empty() {
        return Err(2);
    }
    let port: u16 = match form.port.trim().parse() {
        Ok(port) if port > 0 => port,
        _ => return Err(3),
    };
    if form.public_key.trim().is_empty() {
        return Err(4);
    }
    let party = form.party.trim();
    Ok(PeerEntry {
        participant_id: form.participant_id.trim().to_owned(),
        name: form.name.trim().to_owned(),
        address: form.address.trim().to_owned(),
        port,
        public_key: form.public_key.trim().to_owned(),
        party: (!party.is_empty()).then(|| party.to_owned()),
    })
}

/// Serialize the editor's peer list as the bare JSON array `POST /network-config`
/// expects.
fn peers_to_json(peers: &[PeerEntry]) -> Value {
    Value::Array(
        peers
            .iter()
            .map(|peer| {
                json!({
                    "participant_id": peer.participant_id,
                    "name": peer.name,
                    "address": peer.address,
                    "port": peer.port,
                    "public_key": peer.public_key,
                    "party": peer.party,
                })
            })
            .collect(),
    )
}

/// The placeholder action sent with on-chain (`core_domain`) confirm/execute —
/// the server builds the real choice from `proposal_cid` and ignores this.
fn placeholder_action() -> Value {
    json!({ "type": "governance_set_threshold", "new_threshold": 0 })
}

/// Resolve the package id, members and operator needed to deploy governance
/// core for a party, and build the deployment form with sensible defaults.
fn build_deploy_context(
    client: &mut DecmanClient,
    party_id: &str,
    party_name: &str,
) -> Result<DeployForm> {
    let packages = client.fetch_packages(party_id)?;
    let known = client.fetch_known_members(party_id)?;
    // Operator party is best-effort prefill — a deploy can still be sent without.
    let operator_party = client.fetch_operator_info().unwrap_or_default();

    let mut participant_ids = Vec::new();
    let mut member_parties = Vec::new();
    for KnownMember {
        participant_uid,
        member_party_id,
    } in known
    {
        if let Some(party) = member_party_id.filter(|party| !party.is_empty()) {
            participant_ids.push(participant_uid);
            member_parties.push(party);
        }
    }

    // Default threshold ≈ a 2/3 majority, at least 2 (matches the web frontend).
    let count = i64::try_from(member_parties.len()).unwrap_or(0);
    let threshold = ((count * 2 + 2) / 3).max(2);

    Ok(DeployForm {
        party_id: party_id.to_owned(),
        party_name: party_name.to_owned(),
        package_id: packages.governance_core.unwrap_or_default(),
        operator_party,
        participant_ids,
        member_parties,
        threshold: threshold.to_string(),
        timeout_micros: "86400000000".to_owned(),
        cursor: 0,
    })
}

/// Build the `POST /contracts` body for a governance-core deployment, or a
/// human error describing why it cannot be submitted.
fn build_contracts_request(form: &DeployForm) -> Result<Value, String> {
    if form.package_id.is_empty() {
        return Err(
            "Unknown governance-core package — vet the governance-core DAR first.".to_owned(),
        );
    }
    if form.member_parties.is_empty() {
        return Err("No governance members resolved — peers may be unreachable.".to_owned());
    }
    let threshold: i64 = form
        .threshold
        .trim()
        .parse()
        .map_err(|_| "Threshold must be a whole number".to_owned())?;
    let timeout: i64 = form
        .timeout_micros
        .trim()
        .parse()
        .map_err(|_| "Timeout must be a whole number".to_owned())?;
    // Validate ranges client-side so the error is immediate, not a chain failure.
    let member_count = i64::try_from(form.member_parties.len()).unwrap_or(i64::MAX);
    if !(1..=member_count).contains(&threshold) {
        return Err(format!("Threshold must be between 1 and {member_count}."));
    }
    if timeout <= 0 {
        return Err("Timeout must be a positive number of microseconds.".to_owned());
    }
    Ok(json!({
        "decentralized_party_id": form.party_id,
        "participant_ids": form.participant_ids,
        "participant_parties": form.member_parties,
        "operator_party": form.operator_party,
        "contracts": [{
            "id": "create-governance-rules",
            "name": "GovernanceRules",
            "package_id": form.package_id,
            "module_name": "Governance.Rules",
            "entity_name": "GovernanceRules",
            "fields": [
                { "type": "decentralized_party" },
                { "type": "party_set", "parties": form.member_parties },
                { "type": "governance_threshold", "value": threshold },
                { "type": "rel_time", "microseconds": timeout },
                { "type": "optional", "inner": { "type": "party_set", "parties": [] } }
            ]
        }]
    }))
}

/// The governance type for a party's rules contract, or `None` when the party
/// has no governance rules contract. Mirrors the web frontend's classification.
fn party_governance_type(party: &DecentralizedParty) -> Option<&'static str> {
    party.contracts.iter().find_map(|contract| {
        let template = contract.template_id.as_str();
        if template == "Governance.Rules:GovernanceRules" {
            Some("core_self")
        } else if template.contains("VaultGovernanceRules") || template.contains("VaultGovernance")
        {
            Some("vault")
        } else {
            None
        }
    })
}

/// Current unix time in seconds (0 if the clock is before the epoch).
fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// The select option value one step forward / back from `current`.
fn next_select_value(options: &[SelectOption], current: &str, forward: bool) -> String {
    if options.is_empty() {
        return current.to_owned();
    }
    let index = options
        .iter()
        .position(|option| option.value == current)
        .unwrap_or(0);
    let len = options.len();
    let next = if forward {
        (index + 1) % len
    } else {
        (index + len - 1) % len
    };
    options[next].value.to_owned()
}

/// Handle a key in the composer form. Returns `true` when the form was
/// submitted (Enter on the virtual submit row).
fn composer_key(composer: &mut Composer, key: KeyEvent) -> bool {
    let field_count = composer.fields.len();
    let multiline = composer
        .fields
        .get(composer.cursor)
        .is_some_and(|field| matches!(field.kind, FieldKind::List | FieldKind::Rows(_)));

    match key.code {
        KeyCode::Up => {
            composer.cursor = composer.cursor.saturating_sub(1);
            return false;
        }
        KeyCode::Down | KeyCode::Tab => {
            if composer.cursor < field_count {
                composer.cursor += 1;
            }
            return false;
        }
        KeyCode::Enter if composer.cursor >= field_count => return true,
        // Enter advances to the next field, except in a multi-line field where
        // it inserts a newline (handled below).
        KeyCode::Enter if !multiline => {
            if composer.cursor < field_count {
                composer.cursor += 1;
            }
            return false;
        }
        _ => {}
    }

    if let Some(field) = composer.fields.get_mut(composer.cursor) {
        match &field.kind {
            FieldKind::Bool => {
                if matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                ) {
                    field.value = if field.value == "true" {
                        "false".to_owned()
                    } else {
                        "true".to_owned()
                    };
                }
            }
            FieldKind::Select(options) => match key.code {
                KeyCode::Left => field.value = next_select_value(options, &field.value, false),
                KeyCode::Right | KeyCode::Char(' ') => {
                    field.value = next_select_value(options, &field.value, true);
                }
                _ => {}
            },
            FieldKind::Text | FieldKind::Int | FieldKind::List | FieldKind::Rows(_) => {
                match key.code {
                    KeyCode::Char(c) => field.value.push(c),
                    KeyCode::Backspace => {
                        field.value.pop();
                    }
                    // Only reached for multi-line fields (single-line Enter is
                    // handled above as field navigation).
                    KeyCode::Enter => field.value.push('\n'),
                    _ => {}
                }
            }
        }
    }
    false
}

/// Application state for the decman-cli terminal UI.
pub struct App {
    requests: Sender<Request>,
    updates: Receiver<Update>,
    active_tab: Tab,
    searching: bool,
    overlay: Overlay,
    parties: Vec<DecentralizedParty>,
    parties_status: Status,
    parties_table: TableState,
    parties_query: String,
    peers: Vec<PeerView>,
    peers_status: Status,
    peers_table: TableState,
    dars: Vec<VettedPackageInfo>,
    dars_status: Status,
    dars_table: TableState,
    dars_query: String,
    feed: Vec<FeedItem>,
    feed_status: Status,
    feed_table: TableState,
    /// When set, the full-screen detail view for this party is shown.
    detail: Option<DecentralizedParty>,
    /// Holdings + audit for the open detail view; `None` while still loading.
    detail_data: Option<DetailData>,
    /// Selection state for the audit table in the detail view.
    audit_table: TableState,
    tick: usize,
    can_logout: bool,
    /// The operator party id, prefetched for composer prefill (empty if unknown).
    operator_party: String,
    should_quit: bool,
    should_logout: bool,
}

impl App {
    /// Create a new application driven by the background workers' channels.
    /// `can_logout` enables the logout shortcut (only when launched from a
    /// `config.toml` profile, not the single `.env` profile).
    pub fn new(requests: Sender<Request>, updates: Receiver<Update>, can_logout: bool) -> Self {
        Self {
            requests,
            updates,
            active_tab: Tab::Parties,
            searching: false,
            overlay: Overlay::None,
            parties: Vec::new(),
            parties_status: Status::Loading,
            parties_table: TableState::default(),
            parties_query: String::new(),
            peers: Vec::new(),
            peers_status: Status::Loading,
            peers_table: TableState::default(),
            dars: Vec::new(),
            dars_status: Status::Loading,
            dars_table: TableState::default(),
            dars_query: String::new(),
            feed: Vec::new(),
            feed_status: Status::Loading,
            feed_table: TableState::default(),
            detail: None,
            detail_data: None,
            audit_table: TableState::default(),
            tick: 0,
            can_logout,
            operator_party: String::new(),
            should_quit: false,
            should_logout: false,
        }
    }

    /// Whether the party detail view is open.
    pub fn detail_open(&self) -> bool {
        self.detail.is_some()
    }

    /// Borrow the open party, its holdings/audit, and the audit table state.
    pub fn detail_view(
        &mut self,
    ) -> Option<(&DecentralizedParty, Option<&DetailData>, &mut TableState)> {
        let party = self.detail.as_ref()?;
        Some((party, self.detail_data.as_ref(), &mut self.audit_table))
    }

    /// Number of audit entries currently loaded for the detail view.
    fn audit_len(&self) -> usize {
        self.detail_data
            .as_ref()
            .and_then(|data| data.audit.as_ref().ok())
            .map_or(0, Vec::len)
    }

    /// Whether the logout shortcut is available (config-profile mode).
    pub fn can_logout(&self) -> bool {
        self.can_logout
    }

    /// The currently active tab.
    pub fn active_tab(&self) -> Tab {
        self.active_tab
    }

    /// The active modal overlay (if any).
    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// Frame counter, advanced once per draw — drives the loading spinner.
    pub fn tick(&self) -> usize {
        self.tick
    }

    /// The search/filter hint to render on the panel frame, if any.
    pub fn search_hint(&self) -> Option<(String, bool)> {
        let query = self.current_query();
        if self.searching {
            Some((format!("search: {query}▏"), true))
        } else if !query.is_empty() {
            Some((format!("filter: {query}"), true))
        } else if self.active_tab.searchable() {
            Some(("/ search".to_owned(), false))
        } else {
            None
        }
    }

    /// Borrow the active tab's render data for the UI layer.
    pub fn tab_view(&mut self) -> TabView<'_> {
        match self.active_tab {
            Tab::Parties => TabView::Parties(
                &self.parties_status,
                filter_parties(&self.parties, &self.parties_query),
                &mut self.parties_table,
            ),
            Tab::Peers => TabView::Peers(&self.peers_status, &self.peers, &mut self.peers_table),
            Tab::Dars => TabView::Dars(
                &self.dars_status,
                filter_dars(&self.dars, &self.dars_query),
                &mut self.dars_table,
            ),
            Tab::Workflows => {
                TabView::Workflows(&self.feed_status, &self.feed, &mut self.feed_table)
            }
        }
    }

    /// Run the draw/input loop until the user quits or logs out.
    ///
    /// # Errors
    ///
    /// Returns an error if drawing a frame or reading an input event fails.
    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<Outcome> {
        // Wipe any prior screen (e.g. the login menu) before taking over.
        terminal.clear()?;

        // Prefetch every on-demand tab up front so switching is instant.
        let _ = self.requests.send(Request::Parties);
        let _ = self.requests.send(Request::Dars);
        let _ = self.requests.send(Request::Feed);
        // Prefetch the operator party for composer prefill.
        let _ = self.requests.send(Request::OperatorInfo);

        while !self.should_quit && !self.should_logout {
            self.tick = self.tick.wrapping_add(1);
            self.draw(terminal)?;
            self.handle_events()?;
            self.drain_updates();
        }

        Ok(if self.should_logout {
            Outcome::Logout
        } else {
            Outcome::Quit
        })
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        terminal.draw(|frame| ui::draw(frame, self))?;
        Ok(())
    }

    /// Apply every pending background update to the cache and overlay.
    fn drain_updates(&mut self) {
        while let Ok(update) = self.updates.try_recv() {
            match update {
                Update::Parties(Ok(parties)) => {
                    self.parties = parties;
                    let len = filter_parties(&self.parties, &self.parties_query).len();
                    clamp_selection(&mut self.parties_table, len);
                    self.parties_status = Status::Loaded;
                }
                Update::Parties(Err(error)) => self.parties_status = Status::Error(error),
                Update::Dars(Ok(dars)) => {
                    self.dars = dars;
                    let len = filter_dars(&self.dars, &self.dars_query).len();
                    clamp_selection(&mut self.dars_table, len);
                    self.dars_status = Status::Loaded;
                }
                Update::Dars(Err(error)) => self.dars_status = Status::Error(error),
                Update::Feed(Ok(feed)) => {
                    self.feed = feed;
                    clamp_selection(&mut self.feed_table, self.feed.len());
                    self.feed_status = Status::Loaded;
                }
                // Keep the last feed on a transient poll error; only surface it
                // when there is nothing to show (mirrors the peer-poll behavior).
                Update::Feed(Err(error)) if self.feed.is_empty() => {
                    self.feed_status = Status::Error(error);
                }
                Update::Feed(Err(_)) => {}
                Update::Peers(Ok(peers)) => {
                    self.peers = peers;
                    clamp_selection(&mut self.peers_table, self.peers.len());
                    self.peers_status = Status::Loaded;
                }
                Update::Peers(Err(error)) if self.peers.is_empty() => {
                    self.peers_status = Status::Error(error);
                }
                Update::Peers(Err(_)) => {}
                Update::Compare(result) => {
                    if matches!(self.overlay, Overlay::Busy(_)) {
                        self.overlay = match result {
                            Ok(comparison) => Overlay::Compare {
                                comparison,
                                scroll: 0,
                            },
                            Err(error) => Overlay::Message(error),
                        };
                    }
                }
                Update::Action(Ok(message)) => {
                    if matches!(self.overlay, Overlay::Busy(_)) {
                        self.overlay = Overlay::Message(message);
                    }
                    // Refresh the tabs an action can affect.
                    let _ = self.requests.send(Request::Feed);
                    let _ = self.requests.send(Request::Dars);
                }
                Update::Action(Err(error)) => {
                    if matches!(self.overlay, Overlay::Busy(_)) {
                        self.overlay = Overlay::Message(format!("Failed: {error}"));
                    }
                }
                Update::ChainAudit(result) => {
                    if matches!(self.overlay, Overlay::Busy(_)) {
                        self.overlay = match result {
                            Ok(entries) => Overlay::ChainAudit { entries, scroll: 0 },
                            Err(error) => Overlay::Message(error),
                        };
                    }
                }
                Update::AuthStatus(result) => {
                    if matches!(
                        self.overlay,
                        Overlay::Busy(_) | Overlay::Auth { .. } | Overlay::GrantRights(_)
                    ) {
                        self.overlay = match result {
                            Ok(parties) => Overlay::Auth {
                                parties,
                                selected: 0,
                            },
                            Err(error) => Overlay::Message(error),
                        };
                    }
                }
                Update::Governance(result) => {
                    if matches!(self.overlay, Overlay::Busy(_) | Overlay::Governance(_)) {
                        self.overlay = match result {
                            Ok(view) => Overlay::Governance(view),
                            Err(error) => Overlay::Message(error),
                        };
                    }
                }
                Update::OperatorInfo(party_id) => self.operator_party = party_id,
                Update::DeployContext(result) => {
                    if matches!(self.overlay, Overlay::Busy(_)) {
                        self.overlay = match result {
                            Ok(form) => Overlay::Deploy(form),
                            Err(error) => Overlay::Message(error),
                        };
                    }
                }
                Update::NetworkPeers(result) => {
                    if matches!(self.overlay, Overlay::Busy(_)) {
                        self.overlay = match result {
                            Ok(peers) => Overlay::NetworkEdit(Box::new(NetworkEditState {
                                peers,
                                selected: 0,
                                adding: None,
                            })),
                            Err(error) => Overlay::Message(error),
                        };
                    }
                }
                // Ignore late detail results after the view was closed.
                Update::Detail(data) if self.detail.is_some() => {
                    self.detail_data = Some(data);
                    if self.audit_len() > 0 && self.audit_table.selected().is_none() {
                        self.audit_table.select(Some(0));
                    }
                }
                Update::Detail(_) => {}
            }
        }
    }

    /// Poll for the next terminal event (with a tick timeout) and dispatch it.
    fn handle_events(&mut self) -> Result<()> {
        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            self.on_key(key);
        }

        Ok(())
    }

    /// Dispatch a key press: overlay, then detail, then search, then main view.
    fn on_key(&mut self, key: KeyEvent) {
        if !matches!(self.overlay, Overlay::None) {
            self.on_overlay_key(key);
            return;
        }
        if self.detail.is_some() {
            self.on_detail_key(key);
            return;
        }
        if self.searching {
            self.on_search_key(key);
            return;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.should_quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            // Esc logs out back to the profile menu, or quits in `.env` mode.
            (KeyCode::Esc, _) if self.can_logout => self.should_logout = true,
            (KeyCode::Esc, _) => self.should_quit = true,
            (KeyCode::Char('/'), _) if self.active_tab.searchable() => self.searching = true,
            (KeyCode::Char('r'), _) => self.refresh_active(),
            // View authentication status for all parties (any tab).
            (KeyCode::Char('A'), _) => self.open_auth(),
            (KeyCode::Tab | KeyCode::Right, _) => self.switch_to(self.active_tab.next()),
            (KeyCode::BackTab | KeyCode::Left, _) => self.switch_to(self.active_tab.previous()),
            (KeyCode::Char('1'), _) => self.switch_to(Tab::Parties),
            (KeyCode::Char('2'), _) => self.switch_to(Tab::Peers),
            (KeyCode::Char('3'), _) => self.switch_to(Tab::Dars),
            (KeyCode::Char('4'), _) => self.switch_to(Tab::Workflows),
            (KeyCode::Down | KeyCode::Char('j'), _) => self.select_next(),
            (KeyCode::Up | KeyCode::Char('k'), _) => self.select_previous(),
            // Open the party detail view.
            (KeyCode::Enter, _) if self.active_tab == Tab::Parties => self.open_party_detail(),
            // Start onboarding a new decentralized party.
            (KeyCode::Char('n'), _) if self.active_tab == Tab::Parties => self.open_onboard(),
            // Edit the network peer configuration.
            (KeyCode::Char('e'), _) if self.active_tab == Tab::Peers => self.open_network_edit(),
            // Workflows actions.
            (KeyCode::Char('a'), _) if self.active_tab == Tab::Workflows => {
                self.invitation_action(true);
            }
            (KeyCode::Char('x'), _) if self.active_tab == Tab::Workflows => {
                self.invitation_action(false);
            }
            (KeyCode::Char('d'), _) if self.active_tab == Tab::Workflows => self.dismiss_run(),
            (KeyCode::Char('c'), _) if self.active_tab == Tab::Workflows => self.cancel_run(),
            (KeyCode::Char('t'), _) if self.active_tab == Tab::Workflows => self.retry_run(),
            (KeyCode::Enter, _) if self.active_tab == Tab::Workflows => self.open_feed_detail(),
            // Dars actions.
            (KeyCode::Char('c'), _) if self.active_tab == Tab::Dars => self.start_compare(),
            (KeyCode::Char('u'), _) if self.active_tab == Tab::Dars => self.start_upload(),
            (KeyCode::Char('d'), _) if self.active_tab == Tab::Dars => self.start_distribute(),
            _ => {}
        }
    }

    /// Handle a key while a modal overlay is open.
    fn on_overlay_key(&mut self, key: KeyEvent) {
        /// What an overlay key resolves to, applied after the borrow ends.
        enum Act {
            None,
            Close,
            Distribute,
            Onboard,
            Kick,
            TestAuth,
            GovConfirm,
            GovExecute,
            GovRevoke,
            GovExpire,
            ComposeAction,
            ComposeProposal,
            OpenComposer,
            SubmitComposer,
            SubmitDeploy,
            OpenGrant,
            SubmitGrant,
            SaveNetwork,
        }

        let mut act = Act::None;
        match &mut self.overlay {
            Overlay::PeerSelect { peers, cursor, .. } => match key.code {
                KeyCode::Esc => act = Act::Close,
                KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    if *cursor + 1 < peers.len() {
                        *cursor += 1;
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(peer) = peers.get_mut(*cursor) {
                        peer.checked = !peer.checked;
                    }
                }
                KeyCode::Enter => act = Act::Distribute,
                _ => {}
            },
            Overlay::Compare { scroll, .. }
            | Overlay::Json { scroll, .. }
            | Overlay::FeedDetail { scroll, .. }
            | Overlay::ChainAudit { scroll, .. } => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => act = Act::Close,
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                _ => {}
            },
            Overlay::Onboard(form) => match key.code {
                KeyCode::Esc => act = Act::Close,
                KeyCode::Enter => act = Act::Onboard,
                KeyCode::Up => form.cursor = form.cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => {
                    if form.cursor < form.peers.len() {
                        form.cursor += 1;
                    }
                }
                KeyCode::Char(' ') if !form.on_prefix() => {
                    if let Some(peer) = form.peer_index().and_then(|i| form.peers.get_mut(i)) {
                        peer.checked = !peer.checked;
                    }
                }
                KeyCode::Backspace if form.on_prefix() => {
                    form.prefix.pop();
                }
                KeyCode::Char(c) if form.on_prefix() => form.prefix.push(c),
                _ => {}
            },
            Overlay::Kick(form) => match key.code {
                KeyCode::Esc => act = Act::Close,
                KeyCode::Enter => act = Act::Kick,
                KeyCode::Up | KeyCode::Char('k') => {
                    form.selected = form.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if form.selected + 1 < form.candidates.len() {
                        form.selected += 1;
                    }
                }
                KeyCode::Left | KeyCode::Char('-') => {
                    form.new_threshold = (form.new_threshold - 1).max(1);
                }
                KeyCode::Right | KeyCode::Char('+') => {
                    form.new_threshold = (form.new_threshold + 1).min(form.max_threshold);
                }
                _ => {}
            },
            Overlay::Auth { parties, selected } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => act = Act::Close,
                KeyCode::Char('t') => act = Act::TestAuth,
                KeyCode::Char('g') => act = Act::OpenGrant,
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') if *selected + 1 < parties.len() => {
                    *selected += 1;
                }
                _ => {}
            },
            Overlay::GrantRights(form) => match key.code {
                KeyCode::Esc => act = Act::Close,
                KeyCode::Up => form.cursor = form.cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => {
                    if form.cursor < 2 {
                        form.cursor += 1;
                    }
                }
                KeyCode::Enter if form.cursor >= 2 => act = Act::SubmitGrant,
                KeyCode::Enter => {
                    if form.cursor < 2 {
                        form.cursor += 1;
                    }
                }
                KeyCode::Char(c) => match form.cursor {
                    0 => form.client_id.push(c),
                    1 => form.client_secret.push(c),
                    _ => {}
                },
                KeyCode::Backspace => match form.cursor {
                    0 => {
                        form.client_id.pop();
                    }
                    1 => {
                        form.client_secret.pop();
                    }
                    _ => {}
                },
                _ => {}
            },
            Overlay::Governance(view) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => act = Act::Close,
                KeyCode::Up | KeyCode::Char('k') => {
                    view.selected = view.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if view.selected + 1 < view.items.len() {
                        view.selected += 1;
                    }
                }
                KeyCode::Char('c') => act = Act::GovConfirm,
                KeyCode::Char('e') => act = Act::GovExecute,
                KeyCode::Char('r') => act = Act::GovRevoke,
                KeyCode::Char('x') => act = Act::GovExpire,
                KeyCode::Char('n') => act = Act::ComposeAction,
                KeyCode::Char('p') => act = Act::ComposeProposal,
                _ => {}
            },
            Overlay::ComposerPick(pick) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => act = Act::Close,
                KeyCode::Up => pick.selected = pick.selected.saturating_sub(1),
                KeyCode::Down => {
                    if pick.selected + 1 < pick.options.len() {
                        pick.selected += 1;
                    }
                }
                KeyCode::Enter => act = Act::OpenComposer,
                _ => {}
            },
            Overlay::Composer(composer) => {
                if key.code == KeyCode::Esc {
                    act = Act::Close;
                } else if composer_key(composer, key) {
                    act = Act::SubmitComposer;
                }
            }
            Overlay::Deploy(form) => match key.code {
                KeyCode::Esc => act = Act::Close,
                KeyCode::Up => form.cursor = form.cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => {
                    if form.cursor < 2 {
                        form.cursor += 1;
                    }
                }
                KeyCode::Enter if form.cursor >= 2 => act = Act::SubmitDeploy,
                KeyCode::Enter => {
                    if form.cursor < 2 {
                        form.cursor += 1;
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() => match form.cursor {
                    0 => form.threshold.push(c),
                    1 => form.timeout_micros.push(c),
                    _ => {}
                },
                KeyCode::Backspace => match form.cursor {
                    0 => {
                        form.threshold.pop();
                    }
                    1 => {
                        form.timeout_micros.pop();
                    }
                    _ => {}
                },
                _ => {}
            },
            Overlay::NetworkEdit(state) => match &mut state.adding {
                Some(form) => match key.code {
                    KeyCode::Esc => state.adding = None,
                    KeyCode::Up => form.cursor = form.cursor.saturating_sub(1),
                    KeyCode::Down | KeyCode::Tab if form.cursor < 6 => form.cursor += 1,
                    KeyCode::Enter if form.cursor >= 6 => match validate_peer_form(form) {
                        // Only add a valid peer; otherwise land the cursor on
                        // the first invalid field rather than writing port 0.
                        Ok(peer) => {
                            state.peers.push(peer);
                            state.adding = None;
                        }
                        Err(invalid_field) => form.cursor = invalid_field,
                    },
                    KeyCode::Enter if form.cursor < 6 => form.cursor += 1,
                    KeyCode::Char(c) => match form.cursor {
                        0 => form.participant_id.push(c),
                        1 => form.name.push(c),
                        2 => form.address.push(c),
                        3 => {
                            if c.is_ascii_digit() {
                                form.port.push(c);
                            }
                        }
                        4 => form.public_key.push(c),
                        5 => form.party.push(c),
                        _ => {}
                    },
                    KeyCode::Backspace => match form.cursor {
                        0 => {
                            form.participant_id.pop();
                        }
                        1 => {
                            form.name.pop();
                        }
                        2 => {
                            form.address.pop();
                        }
                        3 => {
                            form.port.pop();
                        }
                        4 => {
                            form.public_key.pop();
                        }
                        5 => {
                            form.party.pop();
                        }
                        _ => {}
                    },
                    _ => {}
                },
                None => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => act = Act::Close,
                    KeyCode::Up | KeyCode::Char('k') => {
                        state.selected = state.selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j')
                        if state.selected + 1 < state.peers.len() =>
                    {
                        state.selected += 1;
                    }
                    KeyCode::Char('a') => state.adding = Some(PeerForm::blank()),
                    KeyCode::Char('d') if state.selected < state.peers.len() => {
                        state.peers.remove(state.selected);
                        state.selected = state.selected.min(state.peers.len().saturating_sub(1));
                    }
                    KeyCode::Char('s') => act = Act::SaveNetwork,
                    _ => {}
                },
            },
            Overlay::Message(_) | Overlay::Busy(_) => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    act = Act::Close;
                }
            }
            Overlay::None => {}
        }

        match act {
            Act::None => {}
            Act::Close => self.overlay = Overlay::None,
            Act::Distribute => self.confirm_distribute(),
            Act::Onboard => self.confirm_onboard(),
            Act::Kick => self.confirm_kick(),
            Act::TestAuth => self.run_auth_test(),
            Act::GovConfirm => self.gov_confirm(),
            Act::GovExecute => self.gov_execute(),
            Act::GovRevoke => self.gov_revoke(),
            Act::GovExpire => self.gov_expire(),
            Act::ComposeAction => self.open_composer_pick(ComposerKind::Action),
            Act::ComposeProposal => self.open_composer_pick(ComposerKind::Proposal),
            Act::OpenComposer => self.open_composer(),
            Act::SubmitComposer => self.submit_composer(),
            Act::SubmitDeploy => self.submit_deploy(),
            Act::OpenGrant => self.open_grant(),
            Act::SubmitGrant => self.submit_grant(),
            Act::SaveNetwork => self.save_network(),
        }
    }

    /// Handle a key press while the search box is focused.
    fn on_search_key(&mut self, key: KeyEvent) {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match key.code {
            KeyCode::Esc => {
                self.set_query(String::new());
                self.searching = false;
                self.reset_selection();
            }
            KeyCode::Enter => self.searching = false,
            KeyCode::Backspace => {
                self.query_pop();
                self.reset_selection();
            }
            KeyCode::Char(c) => {
                self.query_push(c);
                self.reset_selection();
            }
            _ => {}
        }
    }

    /// Handle a key while the party detail view is open.
    fn on_detail_key(&mut self, key: KeyEvent) {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                self.detail = None;
                self.detail_data = None;
                self.audit_table = TableState::default();
            }
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => {
                let len = self.audit_len();
                if len > 0 {
                    let next = self
                        .audit_table
                        .selected()
                        .map_or(0, |i| (i + 1).min(len - 1));
                    self.audit_table.select(Some(next));
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.audit_len() > 0 {
                    let prev = self
                        .audit_table
                        .selected()
                        .map_or(0, |i| i.saturating_sub(1));
                    self.audit_table.select(Some(prev));
                }
            }
            // Open the selected audit entry's JSON in a modal.
            KeyCode::Enter | KeyCode::Char(' ') => self.open_audit_json(),
            // Kick a participant from this party.
            KeyCode::Char('K') => self.open_kick(),
            // View the on-chain governance audit trail.
            KeyCode::Char('c') => self.open_chain_audit(),
            // Open the governance approvals list (confirm / execute / revoke).
            KeyCode::Char('g') => self.open_governance(),
            // Deploy governance-core contracts for this party.
            KeyCode::Char('D') => self.open_deploy(),
            _ => {}
        }
    }

    /// Open the selected audit entry's JSON details in a popup modal.
    fn open_audit_json(&mut self) {
        let value = self
            .detail_data
            .as_ref()
            .and_then(|data| data.audit.as_ref().ok())
            .and_then(|audit| self.audit_table.selected().and_then(|i| audit.get(i)))
            .map(|entry| entry.details.clone());
        if let Some(value) = value {
            self.overlay = Overlay::Json { value, scroll: 0 };
        }
    }

    /// Open the detail view for the selected party.
    fn open_party_detail(&mut self) {
        let party = {
            let filtered = filter_parties(&self.parties, &self.parties_query);
            self.parties_table
                .selected()
                .and_then(|index| filtered.get(index).copied())
                .cloned()
        };
        if let Some(party) = party {
            let _ = self
                .requests
                .send(Request::Detail(party.party_id.to_string()));
            self.detail = Some(party);
            self.detail_data = None;
            self.audit_table = TableState::default();
        }
    }

    /// Accept (`accept = true`) or decline the selected invitation.
    fn invitation_action(&mut self, accept: bool) {
        let id = match self.selected_feed_item() {
            Some(FeedItem::Invitation(invitation)) => invitation.id.clone(),
            _ => return,
        };
        let request = if accept {
            Request::Accept(id)
        } else {
            Request::Decline(id)
        };
        let _ = self.requests.send(request);
        let verb = if accept { "Accepting" } else { "Declining" };
        self.overlay = Overlay::Busy(format!("{verb} invitation…"));
    }

    /// Dismiss the selected finished workflow run.
    fn dismiss_run(&mut self) {
        let name = match self.selected_feed_item() {
            Some(FeedItem::Run(run)) => run.instance_name.clone(),
            _ => return,
        };
        let _ = self.requests.send(Request::Dismiss(name));
        self.overlay = Overlay::Busy("Dismissing workflow…".to_owned());
    }

    /// Cancel the selected workflow run (only valid for an in-progress run this
    /// node coordinates).
    fn cancel_run(&mut self) {
        let kind = match self.selected_feed_item() {
            Some(FeedItem::Run(run))
                if run.status == WorkflowProgress::InProgress
                    && run.role == WorkflowRole::Coordinator =>
            {
                run.kind
            }
            Some(FeedItem::Run(_)) => {
                self.overlay = Overlay::Message(
                    "Only in-progress workflows you coordinate can be cancelled.".to_owned(),
                );
                return;
            }
            _ => return,
        };
        let _ = self.requests.send(Request::CancelWorkflow(kind));
        self.overlay = Overlay::Busy("Cancelling workflow…".to_owned());
    }

    /// Retry the selected workflow run (only valid for a failed run this node
    /// coordinates).
    fn retry_run(&mut self) {
        let name = match self.selected_feed_item() {
            Some(FeedItem::Run(run))
                if run.status == WorkflowProgress::Failed
                    && run.role == WorkflowRole::Coordinator =>
            {
                run.instance_name.clone()
            }
            Some(FeedItem::Run(_)) => {
                self.overlay = Overlay::Message(
                    "Only failed workflows you coordinate can be retried.".to_owned(),
                );
                return;
            }
            _ => return,
        };
        let _ = self.requests.send(Request::RetryWorkflow(name));
        self.overlay = Overlay::Busy("Retrying workflow…".to_owned());
    }

    /// Open the action / proposal type picker from the governance overlay.
    fn open_composer_pick(&mut self, kind: ComposerKind) {
        let pick = {
            let Overlay::Governance(view) = &self.overlay else {
                return;
            };
            if matches!(kind, ComposerKind::Proposal) && view.governance_type != "core_self" {
                None
            } else {
                let options = match kind {
                    ComposerKind::Action => composer::action_types(&view.governance_type),
                    ComposerKind::Proposal => composer::proposal_types(),
                };
                Some(ComposerPick {
                    kind,
                    party_id: view.party_id.clone(),
                    party_name: view.party_name.clone(),
                    governance_type: view.governance_type.clone(),
                    rules_contract_id: view.rules_contract_id.clone(),
                    default_threshold: i64::from(view.threshold),
                    options,
                    selected: 0,
                })
            }
        };
        self.overlay = match pick {
            Some(pick) => Overlay::ComposerPick(Box::new(pick)),
            None => Overlay::Message(
                "Proposals are only available on core-self governance parties.".to_owned(),
            ),
        };
    }

    /// Open the composer form for the type selected in the picker.
    fn open_composer(&mut self) {
        let composer = {
            let Overlay::ComposerPick(pick) = &self.overlay else {
                return;
            };
            let Some(option) = pick.options.get(pick.selected) else {
                return;
            };
            let ctx = ComposerContext {
                party_id: pick.party_id.clone(),
                operator_party: self.operator_party.clone(),
                default_threshold: pick.default_threshold,
            };
            let (fields, submit) = match pick.kind {
                ComposerKind::Action => (
                    composer::fields_for_action(option.key, &ctx),
                    ComposerSubmit::Confirm,
                ),
                ComposerKind::Proposal => (
                    composer::fields_for_proposal(option.key, &ctx),
                    ComposerSubmit::Propose,
                ),
            };
            Composer {
                title: option.label.to_owned(),
                action_type: option.key,
                submit,
                party_id: pick.party_id.clone(),
                party_name: pick.party_name.clone(),
                governance_type: pick.governance_type.clone(),
                rules_contract_id: pick.rules_contract_id.clone(),
                fields,
                cursor: 0,
            }
        };
        self.overlay = Overlay::Composer(Box::new(composer));
    }

    /// Validate the composer form and submit it (confirm or propose).
    fn submit_composer(&mut self) {
        let outcome = {
            let Overlay::Composer(composer) = &self.overlay else {
                return;
            };
            composer::build_payload(composer).map(|payload| {
                let op = match composer.submit {
                    ComposerSubmit::Confirm => GovOp::Confirm {
                        action: payload,
                        governance_type: composer.governance_type.clone(),
                        proposal_cid: None,
                    },
                    ComposerSubmit::Propose => GovOp::Propose { proposal: payload },
                };
                GovActionRequest {
                    op,
                    party_id: composer.party_id.clone(),
                    party_name: composer.party_name.clone(),
                    governance_type: composer.governance_type.clone(),
                    rules_contract_id: composer.rules_contract_id.clone(),
                }
            })
        };
        match outcome {
            Ok(request) => {
                let _ = self.requests.send(Request::GovAction(Box::new(request)));
                self.overlay = Overlay::Busy("Submitting…".to_owned());
            }
            Err(error) => self.overlay = Overlay::Message(error),
        }
    }

    /// Start a governance-core contract deployment for the party in detail view.
    fn open_deploy(&mut self) {
        let Some(party) = self.detail.as_ref() else {
            return;
        };
        if party_governance_type(party).is_some() {
            self.overlay = Overlay::Message(
                "This party already has governance contracts deployed.".to_owned(),
            );
            return;
        }
        let _ = self.requests.send(Request::DeployContext {
            party_id: party.party_id.to_string(),
            party_name: party_name(party).to_owned(),
        });
        self.overlay = Overlay::Busy("Preparing contract deployment…".to_owned());
    }

    /// Validate the deploy form and start the contracts workflow.
    fn submit_deploy(&mut self) {
        let outcome = {
            let Overlay::Deploy(form) = &self.overlay else {
                return;
            };
            build_contracts_request(form)
        };
        match outcome {
            Ok(body) => {
                let _ = self.requests.send(Request::DeploySubmit(Box::new(body)));
                self.overlay = Overlay::Busy("Deploying contracts…".to_owned());
            }
            Err(error) => self.overlay = Overlay::Message(error),
        }
    }

    /// Fetch the peer configuration and open the network-config editor.
    fn open_network_edit(&mut self) {
        let _ = self.requests.send(Request::NetworkPeers);
        self.overlay = Overlay::Busy("Loading network configuration…".to_owned());
    }

    /// Save the edited peer list back to the node.
    fn save_network(&mut self) {
        let body = {
            let Overlay::NetworkEdit(state) = &self.overlay else {
                return;
            };
            peers_to_json(&state.peers)
        };
        let _ = self.requests.send(Request::SaveNetwork(Box::new(body)));
        self.overlay = Overlay::Busy("Saving network configuration…".to_owned());
    }

    /// Fetch and show the per-party authentication status overlay.
    fn open_auth(&mut self) {
        let _ = self.requests.send(Request::AuthStatus);
        self.overlay = Overlay::Busy("Loading authentication status…".to_owned());
    }

    /// Re-test authentication for all parties, then refresh the auth overlay.
    fn run_auth_test(&mut self) {
        let _ = self.requests.send(Request::TestAuth);
        self.overlay = Overlay::Busy("Testing authentication…".to_owned());
    }

    /// Open the grant-rights form for the party selected in the auth overlay.
    fn open_grant(&mut self) {
        let form = {
            let Overlay::Auth { parties, selected } = &self.overlay else {
                return;
            };
            parties.get(*selected).map(|party| GrantForm {
                dec_party_id: party.dec_party_id.clone(),
                party_name: party
                    .dec_party_id
                    .split("::")
                    .next()
                    .unwrap_or(&party.dec_party_id)
                    .to_owned(),
                client_id: String::new(),
                client_secret: String::new(),
                cursor: 0,
            })
        };
        if let Some(form) = form {
            self.overlay = Overlay::GrantRights(Box::new(form));
        }
    }

    /// Submit the grant-rights form, then refresh the auth overlay.
    fn submit_grant(&mut self) {
        let request = {
            let Overlay::GrantRights(form) = &self.overlay else {
                return;
            };
            if form.client_id.trim().is_empty() || form.client_secret.is_empty() {
                None
            } else {
                Some(Request::GrantRights {
                    dec_party_id: form.dec_party_id.clone(),
                    client_id: form.client_id.trim().to_owned(),
                    client_secret: form.client_secret.clone(),
                })
            }
        };
        match request {
            Some(request) => {
                let _ = self.requests.send(request);
                self.overlay = Overlay::Busy("Granting rights…".to_owned());
            }
            None => {
                self.overlay =
                    Overlay::Message("Admin client id and secret are required.".to_owned());
            }
        }
    }

    /// Open the governance approvals list for the party in the detail view.
    fn open_governance(&mut self) {
        let Some(party) = self.detail.as_ref() else {
            return;
        };
        let Some(governance_type) = party_governance_type(party) else {
            self.overlay =
                Overlay::Message("This party has no governance rules contract.".to_owned());
            return;
        };
        let _ = self.requests.send(Request::Governance {
            party_id: party.party_id.to_string(),
            party_name: party_name(party).to_owned(),
            governance_type: governance_type.to_owned(),
        });
        self.overlay = Overlay::Busy("Loading governance approvals…".to_owned());
    }

    /// Build a governance-action request for the selected item, applying `op`.
    fn gov_request(view: &GovView, op: GovOp) -> GovActionRequest {
        GovActionRequest {
            op,
            party_id: view.party_id.clone(),
            party_name: view.party_name.clone(),
            governance_type: view.governance_type.clone(),
            rules_contract_id: view.rules_contract_id.clone(),
        }
    }

    /// Add this node's confirmation to the selected governance action.
    fn gov_confirm(&mut self) {
        let request = {
            let Overlay::Governance(view) = &self.overlay else {
                return;
            };
            let Some(item) = view.items.get(view.selected) else {
                return;
            };
            let op = match item {
                GovItem::OffChain(action) => GovOp::Confirm {
                    action: action.action.clone(),
                    governance_type: view.governance_type.clone(),
                    proposal_cid: None,
                },
                GovItem::Domain(domain) => GovOp::Confirm {
                    action: placeholder_action(),
                    governance_type: "core_domain".to_owned(),
                    proposal_cid: Some(domain.proposal_cid.clone()),
                },
            };
            Self::gov_request(view, op)
        };
        let _ = self.requests.send(Request::GovAction(Box::new(request)));
        self.overlay = Overlay::Busy("Confirming action…".to_owned());
    }

    /// Execute the selected governance action once its threshold is met.
    fn gov_execute(&mut self) {
        let (can_execute, request) = {
            let Overlay::Governance(view) = &self.overlay else {
                return;
            };
            let Some(item) = view.items.get(view.selected) else {
                return;
            };
            let (can_execute, op) = match item {
                GovItem::OffChain(action) => (
                    action.can_execute,
                    GovOp::Execute {
                        action: action.action.clone(),
                        confirmation_cids: confirmation_cids(&action.confirmations),
                        governance_type: view.governance_type.clone(),
                        proposal_cid: None,
                    },
                ),
                GovItem::Domain(domain) => (
                    domain.can_execute,
                    GovOp::Execute {
                        action: placeholder_action(),
                        confirmation_cids: confirmation_cids(&domain.confirmations),
                        governance_type: "core_domain".to_owned(),
                        proposal_cid: Some(domain.proposal_cid.clone()),
                    },
                ),
            };
            (can_execute, Self::gov_request(view, op))
        };
        if !can_execute {
            self.overlay =
                Overlay::Message("Not enough confirmations to execute this action yet.".to_owned());
            return;
        }
        let _ = self.requests.send(Request::GovAction(Box::new(request)));
        self.overlay = Overlay::Busy("Executing action…".to_owned());
    }

    /// Revoke this node's own confirmation on the selected governance action.
    fn gov_revoke(&mut self) {
        let request = {
            let Overlay::Governance(view) = &self.overlay else {
                return;
            };
            let Some(item) = view.items.get(view.selected) else {
                return;
            };
            let (confirmations, governance_type) = match item {
                GovItem::OffChain(action) => (&action.confirmations, view.governance_type.clone()),
                GovItem::Domain(domain) => (&domain.confirmations, "core_domain".to_owned()),
            };
            confirmations
                .iter()
                .find(|confirmation| confirmation.confirming_party == view.member_party_id)
                .map(|mine| {
                    Self::gov_request(
                        view,
                        GovOp::Cancel {
                            confirmation_cid: mine.contract_id.clone(),
                            governance_type,
                        },
                    )
                })
        };
        match request {
            Some(request) => {
                let _ = self.requests.send(Request::GovAction(Box::new(request)));
                self.overlay = Overlay::Busy("Revoking confirmation…".to_owned());
            }
            None => {
                self.overlay = Overlay::Message("You have no confirmation to revoke.".to_owned());
            }
        }
    }

    /// Expire the first stale confirmation on the selected governance action.
    fn gov_expire(&mut self) {
        let now = now_secs();
        let request = {
            let Overlay::Governance(view) = &self.overlay else {
                return;
            };
            let Some(item) = view.items.get(view.selected) else {
                return;
            };
            let (confirmations, governance_type) = match item {
                GovItem::OffChain(action) => (&action.confirmations, view.governance_type.clone()),
                GovItem::Domain(domain) => (&domain.confirmations, "core_domain".to_owned()),
            };
            confirmations
                .iter()
                .find(|confirmation| confirmation.expires_at > 0 && confirmation.expires_at <= now)
                .map(|stale| {
                    Self::gov_request(
                        view,
                        GovOp::Expire {
                            confirmation_cid: stale.contract_id.clone(),
                            governance_type,
                        },
                    )
                })
        };
        match request {
            Some(request) => {
                let _ = self.requests.send(Request::GovAction(Box::new(request)));
                self.overlay = Overlay::Busy("Expiring confirmation…".to_owned());
            }
            None => {
                self.overlay =
                    Overlay::Message("No expired confirmations on this action.".to_owned());
            }
        }
    }

    /// Fetch and show the on-chain governance audit trail for the open party.
    fn open_chain_audit(&mut self) {
        if let Some(party) = self.detail.as_ref() {
            let _ = self
                .requests
                .send(Request::ChainAudit(party.party_id.to_string()));
            self.overlay = Overlay::Busy("Loading on-chain audit…".to_owned());
        }
    }

    /// Open a scrollable detail view for the selected feed item.
    fn open_feed_detail(&mut self) {
        if let Some(item) = self.selected_feed_item() {
            self.overlay = Overlay::FeedDetail {
                item: Box::new(item.clone()),
                scroll: 0,
            };
        }
    }

    /// Open the onboarding form, seeded with all configured peers (self
    /// excluded), each ticked by default — matching the web frontend.
    fn open_onboard(&mut self) {
        let peers: Vec<PeerChoice> = self
            .peers
            .iter()
            .filter(|peer| !peer.is_self)
            .map(|peer| PeerChoice {
                id: peer.participant_id.clone(),
                name: peer.name.clone(),
                checked: true,
            })
            .collect();
        if peers.is_empty() {
            self.overlay = Overlay::Message(
                "No peers available to onboard with. Configure peers first.".to_owned(),
            );
            return;
        }
        self.overlay = Overlay::Onboard(OnboardForm {
            prefix: String::new(),
            peers,
            cursor: 0,
        });
    }

    /// Validate the onboarding form and send the start request.
    fn confirm_onboard(&mut self) {
        let Overlay::Onboard(form) = &self.overlay else {
            return;
        };
        let prefix = form.prefix.trim().to_owned();
        if let Err(message) = validate_prefix(&prefix) {
            self.overlay = Overlay::Message(message);
            return;
        }
        let peer_ids: Vec<String> = form
            .peers
            .iter()
            .filter(|peer| peer.checked)
            .map(|peer| peer.id.clone())
            .collect();
        if peer_ids.is_empty() {
            self.overlay = Overlay::Message("Select at least one peer to onboard.".to_owned());
            return;
        }
        let _ = self.requests.send(Request::Onboard {
            prefix: prefix.clone(),
            peer_ids,
        });
        self.overlay = Overlay::Busy(format!("Starting onboarding for {prefix}…"));
    }

    /// Open the kick form for the party in the detail view, listing the owner
    /// participants that can be removed (self excluded).
    fn open_kick(&mut self) {
        let Some(party) = self.detail.as_ref() else {
            return;
        };
        // Require this node's identity to be resolved before listing candidates,
        // otherwise self could not be excluded and might be kicked by mistake.
        let Some(self_id) = self
            .peers
            .iter()
            .find(|peer| peer.is_self)
            .map(|peer| peer.participant_id.clone())
        else {
            self.overlay = Overlay::Message(
                "Still resolving this node's identity — try again in a moment.".to_owned(),
            );
            return;
        };
        let candidates: Vec<KickCandidate> = party
            .participants
            .iter()
            .filter(|participant| participant.owner_key.is_some())
            .filter_map(|participant| {
                let id = participant.participant_uid.to_string();
                if id == self_id {
                    return None;
                }
                Some(KickCandidate {
                    participant_id: id,
                    label: participant.participant_uid.prefix.clone(),
                })
            })
            .collect();
        if candidates.is_empty() {
            self.overlay = Overlay::Message("No owner participants available to kick.".to_owned());
            return;
        }
        let owners = i32::try_from(party.owners.len()).unwrap_or(i32::MAX);
        let remaining = (owners - 1).max(0);
        if remaining < 1 {
            self.overlay =
                Overlay::Message("Cannot kick: the party would be left with no owners.".to_owned());
            return;
        }
        let suggested = (((remaining + 1) / 2).max(1)).min(remaining);
        self.overlay = Overlay::Kick(KickForm {
            party_id: party.party_id.to_string(),
            party_name: party_name(party).to_owned(),
            previous_threshold: party.threshold,
            candidates,
            selected: 0,
            new_threshold: suggested,
            max_threshold: remaining,
        });
    }

    /// Send the kick request for the selected candidate in the kick form.
    fn confirm_kick(&mut self) {
        let Overlay::Kick(form) = &self.overlay else {
            return;
        };
        let Some(candidate) = form.candidates.get(form.selected) else {
            return;
        };
        let request = Request::Kick {
            party_id: form.party_id.clone(),
            participant_id: candidate.participant_id.clone(),
            new_threshold: form.new_threshold,
            previous_threshold: form.previous_threshold,
        };
        let label = candidate.label.clone();
        let _ = self.requests.send(request);
        self.overlay = Overlay::Busy(format!("Kicking {label}…"));
    }

    /// Kick off the package checker.
    fn start_compare(&mut self) {
        let _ = self.requests.send(Request::Compare);
        self.overlay = Overlay::Busy("Comparing packages with peers…".to_owned());
    }

    /// Pick a DAR and upload it to this node.
    fn start_upload(&mut self) {
        if let Some(dar) = self.pick_dar() {
            let _ = self.requests.send(Request::Upload {
                filename: dar.filename,
                data: dar.data,
            });
            self.overlay = Overlay::Busy("Uploading DAR…".to_owned());
        }
    }

    /// Pick a DAR, then open the peer-selection dialog for distribution.
    fn start_distribute(&mut self) {
        let Some(dar) = self.pick_dar() else {
            return;
        };
        let peers: Vec<PeerChoice> = self
            .peers
            .iter()
            .filter(|peer| !peer.is_self)
            .map(|peer| PeerChoice {
                id: peer.participant_id.clone(),
                name: peer.name.clone(),
                checked: false,
            })
            .collect();
        if peers.is_empty() {
            self.overlay = Overlay::Message("No peers available to distribute to.".to_owned());
            return;
        }
        self.overlay = Overlay::PeerSelect {
            dar,
            peers,
            cursor: 0,
        };
    }

    /// Send the distribution request for the checked peers in the dialog.
    fn confirm_distribute(&mut self) {
        let Overlay::PeerSelect { dar, peers, .. } = &self.overlay else {
            return;
        };
        let peer_ids: Vec<String> = peers
            .iter()
            .filter(|peer| peer.checked)
            .map(|peer| peer.id.clone())
            .collect();
        if peer_ids.is_empty() {
            return;
        }
        let (filename, data) = (dar.filename.clone(), dar.data.clone());
        let _ = self.requests.send(Request::Distribute {
            filename,
            data,
            peer_ids,
        });
        self.overlay = Overlay::Busy("Distributing DAR…".to_owned());
    }

    /// Open the native file dialog and read the chosen DAR as base64.
    fn pick_dar(&mut self) -> Option<Dar> {
        let path = rfd::FileDialog::new()
            .add_filter("DAR files", &["dar"])
            .pick_file()?;
        match std::fs::read(&path) {
            Ok(bytes) => {
                let filename = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "upload.dar".to_owned());
                Some(Dar {
                    filename,
                    data: BASE64_STANDARD.encode(bytes),
                })
            }
            Err(error) => {
                self.overlay = Overlay::Message(format!("Failed to read DAR: {error}"));
                None
            }
        }
    }

    /// Request a fresh fetch of the active tab (peers refresh on their own).
    fn refresh_active(&mut self) {
        if let Some(request) = self.active_tab.request() {
            let _ = self.requests.send(request);
        }
    }

    /// Switch tabs. Data is prefetched and cached, so this is instant.
    fn switch_to(&mut self, tab: Tab) {
        if self.active_tab == tab {
            return;
        }
        self.searching = false;
        self.active_tab = tab;
    }

    /// The selected item on the Workflows feed.
    fn selected_feed_item(&self) -> Option<&FeedItem> {
        let index = self.feed_table.selected()?;
        self.feed.get(index)
    }

    fn current_query(&self) -> &str {
        match self.active_tab {
            Tab::Parties => &self.parties_query,
            Tab::Dars => &self.dars_query,
            _ => "",
        }
    }

    fn current_query_mut(&mut self) -> Option<&mut String> {
        match self.active_tab {
            Tab::Parties => Some(&mut self.parties_query),
            Tab::Dars => Some(&mut self.dars_query),
            _ => None,
        }
    }

    fn query_push(&mut self, c: char) {
        if let Some(query) = self.current_query_mut() {
            query.push(c);
        }
    }

    fn query_pop(&mut self) {
        if let Some(query) = self.current_query_mut() {
            query.pop();
        }
    }

    fn set_query(&mut self, value: String) {
        if let Some(query) = self.current_query_mut() {
            *query = value;
        }
    }

    fn current_len(&self) -> usize {
        match self.active_tab {
            Tab::Parties => filter_parties(&self.parties, &self.parties_query).len(),
            Tab::Peers => self.peers.len(),
            Tab::Dars => filter_dars(&self.dars, &self.dars_query).len(),
            Tab::Workflows => self.feed.len(),
        }
    }

    fn current_table_mut(&mut self) -> &mut TableState {
        match self.active_tab {
            Tab::Parties => &mut self.parties_table,
            Tab::Peers => &mut self.peers_table,
            Tab::Dars => &mut self.dars_table,
            Tab::Workflows => &mut self.feed_table,
        }
    }

    /// Reset the active tab's selection to the first row (or none when empty).
    fn reset_selection(&mut self) {
        let len = self.current_len();
        self.current_table_mut().select((len > 0).then_some(0));
    }

    /// Move the selection to the next row, wrapping at the end.
    fn select_next(&mut self) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let table = self.current_table_mut();
        let next = table.selected().map_or(0, |i| (i + 1) % len);
        table.select(Some(next));
    }

    /// Move the selection to the previous row, wrapping at the start.
    fn select_previous(&mut self) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let table = self.current_table_mut();
        let previous = table.selected().map_or(0, |i| (i + len - 1) % len);
        table.select(Some(previous));
    }
}

#[cfg(test)]
mod tests {
    use common::canton_id::CantonId;

    use super::*;

    /// A valid 34-byte (68 hex char) namespace for building realistic Canton ids.
    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn party(name: &str) -> DecentralizedParty {
        DecentralizedParty {
            party_id: CantonId::parse(&format!("{name}::{NS}")).unwrap(),
            threshold: 1,
            owners: Vec::new(),
            my_owner_key: None,
            participants: Vec::new(),
            contracts: Vec::new(),
            local_metadata: None,
        }
    }

    #[test]
    fn filter_parties_matches_name_case_insensitively() {
        let parties = [party("cbtc-network"), party("vault-rc5"), party("test-net")];

        let all = filter_parties(&parties, "");
        let vault = filter_parties(&parties, "VAULT");

        assert_eq!(all.len(), 3);
        assert_eq!(vault.len(), 1);
        assert_eq!(party_name(vault[0]), "vault-rc5");
    }

    #[test]
    fn filter_parties_matches_party_id() {
        let parties = [party("alpha")];
        // A substring of the namespace hex matches against the full party id.
        assert_eq!(filter_parties(&parties, "c4010d").len(), 1);
        assert_eq!(filter_parties(&parties, "nomatch").len(), 0);
    }

    #[test]
    fn validate_prefix_accepts_valid_and_rejects_invalid() {
        assert!(validate_prefix("vault-rc5").is_ok());
        assert!(validate_prefix("a_b-9").is_ok());
        assert!(validate_prefix("").is_err()); // empty
        assert!(validate_prefix("9abc").is_err()); // must start with a letter
        assert!(validate_prefix("ab cd").is_err()); // space
        assert!(validate_prefix("ab.cd").is_err()); // illegal char
        assert!(validate_prefix(&"a".repeat(181)).is_err()); // too long
        assert!(validate_prefix(&"a".repeat(180)).is_ok()); // at the limit
    }

    fn deploy_form() -> DeployForm {
        DeployForm {
            party_id: "dec::1220".to_owned(),
            party_name: "cbtc-network".to_owned(),
            package_id: "#governance-core-v1".to_owned(),
            operator_party: "op::1220".to_owned(),
            participant_ids: vec!["p1::1220".to_owned(), "p2::1220".to_owned()],
            member_parties: vec!["m1::1220".to_owned(), "m2::1220".to_owned()],
            threshold: "2".to_owned(),
            timeout_micros: "86400000000".to_owned(),
            cursor: 0,
        }
    }

    #[test]
    fn build_contracts_request_assembles_gov_core() {
        let body = match build_contracts_request(&deploy_form()) {
            Ok(body) => body,
            Err(error) => panic!("build failed: {error}"),
        };
        assert_eq!(body["decentralized_party_id"], "dec::1220");
        assert_eq!(body["participant_ids"][0], "p1::1220");
        assert_eq!(body["participant_parties"][1], "m2::1220");
        let contract = &body["contracts"][0];
        assert_eq!(contract["entity_name"], "GovernanceRules");
        assert_eq!(contract["fields"][1]["type"], "party_set");
        assert_eq!(contract["fields"][2]["type"], "governance_threshold");
        assert_eq!(contract["fields"][2]["value"], 2);
        assert_eq!(contract["fields"][3]["microseconds"], 86_400_000_000_i64);
    }

    #[test]
    fn build_contracts_request_rejects_missing_package() {
        let mut form = deploy_form();
        form.package_id = String::new();
        match build_contracts_request(&form) {
            Err(error) => assert!(error.contains("governance-core")),
            Ok(_) => panic!("expected a missing-package error"),
        }
    }

    #[test]
    fn build_contracts_request_rejects_out_of_range_threshold() {
        // Two members, so a threshold of 3 is out of the 1..=2 range.
        let mut form = deploy_form();
        form.threshold = "3".to_owned();
        match build_contracts_request(&form) {
            Err(error) => assert!(error.contains("between 1 and 2")),
            Ok(_) => panic!("expected an out-of-range threshold error"),
        }

        let mut form = deploy_form();
        form.timeout_micros = "0".to_owned();
        match build_contracts_request(&form) {
            Err(error) => assert!(error.contains("positive")),
            Ok(_) => panic!("expected a non-positive timeout error"),
        }
    }

    #[test]
    fn validate_peer_form_rejects_blank_and_zero_port() {
        let full = PeerForm {
            participant_id: "alpha::1220".to_owned(),
            name: "alpha".to_owned(),
            address: "10.0.0.1".to_owned(),
            port: "9001".to_owned(),
            public_key: "abcd".to_owned(),
            party: String::new(),
            cursor: 0,
        };
        match validate_peer_form(&full) {
            Ok(peer) => assert_eq!(peer.port, 9001),
            Err(field) => panic!("expected a valid peer, got invalid field {field}"),
        }

        // Blank port → cursor should land on the port field (index 3).
        let mut blank_port = full;
        blank_port.port = String::new();
        assert!(matches!(validate_peer_form(&blank_port), Err(3)));

        // Missing participant id → field 0.
        let mut no_id = PeerForm::blank();
        no_id.address = "10.0.0.1".to_owned();
        no_id.port = "9001".to_owned();
        no_id.public_key = "abcd".to_owned();
        assert!(matches!(validate_peer_form(&no_id), Err(0)));
    }

    #[test]
    fn clamp_selection_preserves_or_resets() {
        let mut table = TableState::default();

        clamp_selection(&mut table, 0);
        assert_eq!(table.selected(), None);

        clamp_selection(&mut table, 3);
        assert_eq!(table.selected(), Some(0));

        table.select(Some(2));
        clamp_selection(&mut table, 3);
        assert_eq!(table.selected(), Some(2));

        clamp_selection(&mut table, 1);
        assert_eq!(table.selected(), Some(0));
    }
}
