//! The App state, its accessors, the run loop and update handling.

use std::sync::mpsc::{Receiver, Sender};

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use common::types::{DecentralizedParty, VettedPackageInfo};

use crate::api::{DiscoverResult, FeedItem, PeerView};
use crate::composer::{FieldKind, PickerOption, PickerSource};
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
            self.apply_update(update);
        }
    }

    /// Apply one background update to the cache and overlay.
    fn apply_update(&mut self, update: Update) {
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
            Update::Compare(result) => self.show_result(result, |comparison| Overlay::Compare {
                comparison,
                scroll: 0,
            }),
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
                self.show_result(result, |entries| Overlay::ChainAudit { entries, scroll: 0 });
            }
            Update::AuthStatus(result) => {
                if matches!(
                    self.overlay,
                    Overlay::Busy(_) | Overlay::Auth { .. } | Overlay::GrantRights(_)
                ) {
                    self.overlay = result_overlay(result, |parties| Overlay::Auth {
                        parties,
                        selected: 0,
                    });
                }
            }
            Update::Governance(result) => {
                if matches!(self.overlay, Overlay::Busy(_) | Overlay::Governance(_)) {
                    self.overlay = result_overlay(result, Overlay::Governance);
                }
            }
            Update::OperatorInfo(party_id) => self.operator_party = party_id,
            Update::DeployContext(result) => self.show_result(result, Overlay::Deploy),
            Update::NetworkPeers(result) => self.show_result(result, |peers| {
                Overlay::NetworkEdit(Box::new(NetworkEditState {
                    peers,
                    selected: 0,
                    adding: None,
                }))
            }),
            Update::PartyConfig(result) => self.show_result(result, Overlay::PartyConfig),
            Update::Discovered(result) => self.apply_discovered(result),
            Update::NodeInfo(data) => {
                if matches!(self.overlay, Overlay::Busy(_)) {
                    self.overlay = Overlay::NodeInfo(data);
                }
            }
            Update::PickerOptions(results) => self.apply_picker_options(results),
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

    /// Replace a Busy overlay with `result`. A result that arrives after the
    /// operator closed the overlay is dropped.
    fn show_result<T>(
        &mut self,
        result: Result<T, String>,
        into_overlay: impl FnOnce(T) -> Overlay,
    ) {
        if matches!(self.overlay, Overlay::Busy(_)) {
            self.overlay = result_overlay(result, into_overlay);
        }
    }

    /// Fill the open party-config form with a discover result.
    fn apply_discovered(&mut self, result: Result<DiscoverResult, String>) {
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
                                "Authenticated, but no primary party assigned.".to_owned();
                        }
                    }
                }
                Err(error) => form.status = format!("Discover failed: {error}"),
            }
        }
    }

    /// Fill in the open composer's picker fields. A failed fetch is treated
    /// as an empty list: the field is marked loaded and falls back to
    /// free-text cid entry.
    fn apply_picker_options(
        &mut self,
        results: Vec<(PickerSource, Result<Vec<PickerOption>, String>)>,
    ) {
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
}

/// The overlay that shows `result`: `into_overlay` for a value, the error as
/// a message otherwise.
fn result_overlay<T>(
    result: Result<T, String>,
    into_overlay: impl FnOnce(T) -> Overlay,
) -> Overlay {
    match result {
        Ok(value) => into_overlay(value),
        Err(error) => Overlay::Message(error),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use common::types::PeerPackageComparison;

    use super::*;
    use crate::app::overlay::PartyConfigForm;
    use crate::app::overlay::{DeployForm, GovView, GrantForm, IdpMode, NodeInfoData};
    use crate::composer::{Composer, ComposerField, ComposerSubmit};

    /// Deliver `update` to an app that shows `overlay`. Returns the app and
    /// the requests it sent.
    fn apply(overlay: Overlay, update: Update) -> (App, Receiver<Request>) {
        let (requests, request_rx) = mpsc::channel();
        let (update_tx, updates) = mpsc::channel();
        let mut app = App::new(requests, updates, false);
        app.overlay = overlay;
        if update_tx.send(update).is_err() {
            panic!("the app holds the update receiver");
        }
        app.drain_updates();
        (app, request_rx)
    }

    fn busy() -> Overlay {
        Overlay::Busy("Working…".to_owned())
    }

    fn message(app: &App) -> Option<&str> {
        match &app.overlay {
            Overlay::Message(text) => Some(text),
            _ => None,
        }
    }

    fn gov_view() -> GovView {
        GovView {
            party_name: "treasury".to_owned(),
            party_id: "dec::1220".to_owned(),
            rules_contract_id: "00rules".to_owned(),
            member_party_id: "me::1220".to_owned(),
            threshold: 2,
            items: Vec::new(),
            selected: 0,
        }
    }

    fn party_config_form() -> PartyConfigForm {
        PartyConfigForm {
            dec_party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            mode: IdpMode::Keycloak,
            member_party_id: String::new(),
            user_id: String::new(),
            kc_url: String::new(),
            kc_realm: String::new(),
            kc_client_id: String::new(),
            kc_secret: String::new(),
            kc_has_secret: false,
            auth0_domain: String::new(),
            auth0_audience: String::new(),
            auth0_client_id: String::new(),
            auth0_secret: String::new(),
            auth0_has_secret: false,
            cursor: 0,
            status: String::new(),
        }
    }

    #[test]
    fn a_busy_overlay_shows_the_result_it_waited_for() {
        let comparison = PeerPackageComparison {
            local_packages: Vec::new(),
            peers: Vec::new(),
        };
        let (app, _) = apply(busy(), Update::Compare(Ok(comparison)));
        assert!(matches!(app.overlay, Overlay::Compare { scroll: 0, .. }));
        let (app, _) = apply(busy(), Update::ChainAudit(Ok(Vec::new())));
        assert!(matches!(
            &app.overlay,
            Overlay::ChainAudit { entries, scroll: 0 } if entries.is_empty()
        ));
        let (app, _) = apply(busy(), Update::NetworkPeers(Ok(Vec::new())));
        assert!(matches!(
            &app.overlay,
            Overlay::NetworkEdit(state)
                if state.peers.is_empty() && state.selected == 0 && state.adding.is_none()
        ));
        let (app, _) = apply(
            busy(),
            Update::PartyConfig(Ok(Box::new(party_config_form()))),
        );
        assert!(matches!(&app.overlay, Overlay::PartyConfig(form) if form.party_name == "dec"));
        let (app, _) = apply(busy(), Update::Governance(Ok(Box::new(gov_view()))));
        assert!(matches!(&app.overlay, Overlay::Governance(view) if view.threshold == 2));
        let deploy = DeployForm {
            party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            package_id: String::new(),
            operator_party: String::new(),
            participant_ids: Vec::new(),
            member_parties: Vec::new(),
            threshold: "2".to_owned(),
            timeout_micros: "1".to_owned(),
            cursor: 0,
        };
        let (app, _) = apply(busy(), Update::DeployContext(Ok(Box::new(deploy))));
        assert!(matches!(&app.overlay, Overlay::Deploy(form) if form.threshold == "2"));
        let node_info = NodeInfoData {
            key: Err("no key".to_owned()),
            network: Err("no network".to_owned()),
        };
        let (app, _) = apply(busy(), Update::NodeInfo(Box::new(node_info)));
        assert!(matches!(app.overlay, Overlay::NodeInfo(_)));

        for update in [
            Update::Compare(Err("failed".to_owned())),
            Update::ChainAudit(Err("failed".to_owned())),
            Update::NetworkPeers(Err("failed".to_owned())),
            Update::DeployContext(Err("failed".to_owned())),
            Update::PartyConfig(Err("failed".to_owned())),
            Update::AuthStatus(Err("failed".to_owned())),
            Update::Governance(Err("failed".to_owned())),
        ] {
            let (app, _) = apply(busy(), update);
            assert_eq!(message(&app), Some("failed"));
        }
    }

    #[test]
    fn a_result_after_the_overlay_closed_is_dropped() {
        for update in [
            Update::Compare(Err("late".to_owned())),
            Update::ChainAudit(Ok(Vec::new())),
            Update::NetworkPeers(Err("late".to_owned())),
            Update::DeployContext(Err("late".to_owned())),
            Update::PartyConfig(Err("late".to_owned())),
            Update::AuthStatus(Err("late".to_owned())),
            Update::Governance(Err("late".to_owned())),
            Update::Action(Err("late".to_owned())),
            Update::Discovered(Err("late".to_owned())),
            Update::PickerOptions(Vec::new()),
        ] {
            let (app, _) = apply(Overlay::None, update);
            assert!(matches!(app.overlay, Overlay::None));
        }
        // A form the operator opened since is not replaced either.
        let (app, _) = apply(
            Overlay::Message("kept".to_owned()),
            Update::Compare(Err("late".to_owned())),
        );
        assert_eq!(message(&app), Some("kept"));
    }

    #[test]
    fn auth_and_governance_results_replace_their_own_overlay() {
        let auth = Overlay::Auth {
            parties: Vec::new(),
            selected: 3,
        };
        let (app, _) = apply(auth, Update::AuthStatus(Ok(Vec::new())));
        assert!(matches!(app.overlay, Overlay::Auth { selected: 0, .. }));
        let grant = GrantForm {
            dec_party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            client_id: String::new(),
            client_secret: String::new(),
            cursor: 0,
        };
        let (app, _) = apply(
            Overlay::GrantRights(Box::new(grant)),
            Update::AuthStatus(Err("denied".to_owned())),
        );
        assert_eq!(message(&app), Some("denied"));
        let (app, _) = apply(
            Overlay::Governance(Box::new(gov_view())),
            Update::Governance(Err("gone".to_owned())),
        );
        assert_eq!(message(&app), Some("gone"));
    }

    #[test]
    fn an_action_result_shows_and_refreshes_the_tabs() {
        let (app, requests) = apply(busy(), Update::Action(Ok("Kick started".to_owned())));
        assert_eq!(message(&app), Some("Kick started"));
        assert!(matches!(requests.try_recv(), Ok(Request::Feed)));
        assert!(matches!(requests.try_recv(), Ok(Request::Dars)));
        assert!(requests.try_recv().is_err());

        // The tabs refresh even when the operator closed the overlay.
        let (app, requests) = apply(Overlay::None, Update::Action(Ok("done".to_owned())));
        assert!(matches!(app.overlay, Overlay::None));
        assert!(matches!(requests.try_recv(), Ok(Request::Feed)));
        assert!(matches!(requests.try_recv(), Ok(Request::Dars)));

        let (app, requests) = apply(busy(), Update::Action(Err("refused".to_owned())));
        assert_eq!(message(&app), Some("Failed: refused"));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn discovered_fills_the_open_party_config_form() {
        let status_after = |result| {
            let (app, _) = apply(
                Overlay::PartyConfig(Box::new(party_config_form())),
                Update::Discovered(result),
            );
            match app.overlay {
                Overlay::PartyConfig(form) => (form.user_id, form.member_party_id, form.status),
                _ => panic!("the party config form stays open"),
            }
        };
        let found = status_after(Ok(DiscoverResult {
            user_id: "user-1".to_owned(),
            primary_party: Some("member::1220".to_owned()),
        }));
        assert_eq!(
            found,
            (
                "user-1".to_owned(),
                "member::1220".to_owned(),
                "Discovered member party.".to_owned()
            )
        );
        let no_party = status_after(Ok(DiscoverResult {
            user_id: "user-2".to_owned(),
            primary_party: None,
        }));
        assert_eq!(
            no_party,
            (
                "user-2".to_owned(),
                String::new(),
                "Authenticated, but no primary party assigned.".to_owned()
            )
        );
        let failed = status_after(Err("bad secret".to_owned()));
        assert_eq!(failed.2, "Discover failed: bad secret");
    }

    #[test]
    fn picker_options_fill_the_matching_composer_fields() {
        let picker = |key, source| ComposerField {
            key,
            label: key,
            kind: FieldKind::Picker {
                source,
                options: Vec::new(),
                loaded: false,
            },
            value: String::new(),
            optional: false,
            help: "",
        };
        let composer = Composer {
            title: "Mint".to_owned(),
            action_type: "mint",
            submit: ComposerSubmit::Propose,
            party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            rules_contract_id: "00rules".to_owned(),
            fields: vec![
                picker("instrument", PickerSource::Instruments),
                picker("service", PickerSource::UserServices),
                picker("offer", PickerSource::CredentialOffers),
            ],
            cursor: 0,
        };
        let option = PickerOption {
            value: "00cid".to_owned(),
            label: "one".to_owned(),
        };
        let (app, _) = apply(
            Overlay::Composer(Box::new(composer)),
            Update::PickerOptions(vec![
                (PickerSource::Instruments, Ok(vec![option])),
                (PickerSource::UserServices, Err("down".to_owned())),
            ]),
        );
        let Overlay::Composer(composer) = &app.overlay else {
            panic!("the composer stays open");
        };
        let state: Vec<(usize, bool)> = composer
            .fields
            .iter()
            .map(|field| match &field.kind {
                FieldKind::Picker {
                    options, loaded, ..
                } => (options.len(), *loaded),
                _ => panic!("every field is a picker"),
            })
            .collect();
        // A failed fetch still marks its fields loaded, with no options.
        assert_eq!(state, vec![(1, true), (0, true), (0, false)]);
    }
}
