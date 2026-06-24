use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use base64::prelude::*;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;
use serde_json::Value;

use common::types::{AuditLogEntry, DecentralizedParty, PeerPackageComparison, VettedPackageInfo};

use crate::api::{AuthSettings, DecmanClient, FeedItem, Holding, PeerView, party_name};
use crate::config::Profile;
use crate::ui;

/// Per-party detail extras (holdings + audit), each fetched independently so
/// one failing does not hide the other.
pub struct DetailData {
    pub holdings: Result<Vec<Holding>, String>,
    pub audit: Result<Vec<AuditLogEntry>, String>,
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
            let result = client.fetch_peers().map_err(|error| format!("{error:#}"));
            if peer_tx.send(Update::Peers(result)).is_err() {
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
        }),
    }
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
                Update::Feed(Err(error)) => self.feed_status = Status::Error(error),
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
            // Workflows actions.
            (KeyCode::Char('a'), _) if self.active_tab == Tab::Workflows => {
                self.invitation_action(true);
            }
            (KeyCode::Char('x'), _) if self.active_tab == Tab::Workflows => {
                self.invitation_action(false);
            }
            (KeyCode::Char('d'), _) if self.active_tab == Tab::Workflows => self.dismiss_run(),
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
            Overlay::Compare { scroll, .. } | Overlay::Json { scroll, .. } => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => act = Act::Close,
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                _ => {}
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
