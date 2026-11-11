//! The App state, its accessors, the run loop and update handling.

use std::sync::mpsc::{Receiver, Sender};

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use common::types::{DecentralizedParty, VettedPackageInfo};

use crate::api::{FeedItem, PeerView};
use crate::composer::FieldKind;
use crate::ui;

use crate::app::login::Outcome;
use crate::app::overlay::{DetailData, NetworkEditState, Overlay};
use crate::app::tabs::{Status, Tab, TabView, clamp_selection, filter_dars, filter_parties};
use crate::app::worker::{Request, Update};

/// Application state for the decman-cli terminal UI.
pub struct App {
    pub(super) requests: Sender<Request>,
    updates: Receiver<Update>,
    pub(super) active_tab: Tab,
    pub(super) searching: bool,
    pub(super) overlay: Overlay,
    pub(super) parties: Vec<DecentralizedParty>,
    parties_status: Status,
    pub(super) parties_table: TableState,
    pub(super) parties_query: String,
    pub(super) peers: Vec<PeerView>,
    peers_status: Status,
    pub(super) peers_table: TableState,
    pub(super) dars: Vec<VettedPackageInfo>,
    dars_status: Status,
    pub(super) dars_table: TableState,
    pub(super) dars_query: String,
    pub(super) feed: Vec<FeedItem>,
    feed_status: Status,
    pub(super) feed_table: TableState,
    /// When set, the full-screen detail view for this party is shown.
    pub(super) detail: Option<DecentralizedParty>,
    /// Holdings + audit for the open detail view; `None` while still loading.
    pub(super) detail_data: Option<DetailData>,
    /// Selection state for the audit table in the detail view.
    pub(super) audit_table: TableState,
    tick: usize,
    pub(super) can_logout: bool,
    /// The operator party id, prefetched for composer prefill (empty if unknown).
    pub(super) operator_party: String,
    /// The active tab panel's rect from the last frame, used to hit-test mouse
    /// clicks (tab bar on the top border, table rows inside).
    pub(super) body_area: Rect,
    pub(super) should_quit: bool,
    pub(super) should_logout: bool,
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
            body_area: Rect::default(),
            should_quit: false,
            should_logout: false,
        }
    }

    /// Record the active tab panel's rect (called by the UI each frame) so mouse
    /// clicks can be mapped back to a tab or table row.
    pub fn set_body_area(&mut self, area: Rect) {
        self.body_area = area;
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
    pub(super) fn audit_len(&self) -> usize {
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

    /// The version of the node this session is managing, read off this node's
    /// own peer row (`/node-config`). `None` until the first peer poll returns.
    pub fn node_version(&self) -> Option<&str> {
        self.peers
            .iter()
            .find(|peer| peer.is_self)
            .and_then(|peer| peer.version.as_deref())
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
                Update::PartyConfig(result) => {
                    if matches!(self.overlay, Overlay::Busy(_)) {
                        self.overlay = match result {
                            Ok(form) => Overlay::PartyConfig(form),
                            Err(error) => Overlay::Message(error),
                        };
                    }
                }
                Update::Discovered(result) => {
                    if let Overlay::PartyConfig(form) = &mut self.overlay {
                        match result {
                            Ok(discovered) => {
                                form.user_id = discovered.user_id;
                                match discovered.primary_party {
                                    Some(party) => {
                                        form.member_party_id = party;
                                        form.status = "Discovered member party.".to_owned();
                                    }
                                    None => {
                                        form.status =
                                            "Authenticated, but no primary party assigned."
                                                .to_owned();
                                    }
                                }
                            }
                            Err(error) => form.status = format!("Discover failed: {error}"),
                        }
                    }
                }
                Update::NodeInfo(data) => {
                    if matches!(self.overlay, Overlay::Busy(_)) {
                        self.overlay = Overlay::NodeInfo(data);
                    }
                }
                // Fill in the open composer's picker fields. A failed fetch is
                // treated as an empty list: the field is marked loaded and
                // falls back to free-text cid entry.
                Update::PickerOptions(results) => {
                    if let Overlay::Composer(composer) = &mut self.overlay {
                        for (source, result) in results {
                            let loaded_options = result.unwrap_or_default();
                            for field in &mut composer.fields {
                                if let FieldKind::Picker {
                                    source: field_source,
                                    options,
                                    loaded,
                                } = &mut field.kind
                                    && *field_source == source
                                {
                                    *options = loaded_options.clone();
                                    *loaded = true;
                                }
                            }
                        }
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
}
