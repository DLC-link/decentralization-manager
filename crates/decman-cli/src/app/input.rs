//! Keyboard and mouse dispatch outside the overlays.

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::widgets::TableState;

use crate::app::overlay::Overlay;
use crate::app::state::App;
use crate::app::tabs::{Tab, tab_at};

/// Longest the input loop blocks before redrawing, so background updates
/// (loaded data, live peer latency, spinner) appear without a keypress.
const TICK: Duration = Duration::from_millis(200);

impl App {
    /// Poll for the next terminal event (with a tick timeout) and dispatch it.
    pub(super) fn handle_events(&mut self) -> Result<()> {
        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
                Event::Mouse(mouse) => self.on_mouse(mouse),
                _ => {}
            }
        }

        Ok(())
    }

    /// Dispatch a mouse event. The scroll wheel maps to the up/down keys so it
    /// drives navigation in every context; a left click on the main view selects
    /// a tab or table row.
    fn on_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            }
            MouseEventKind::ScrollUp => self.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            // Clicks only act on the plain tab view — overlays, the detail view
            // and search stay keyboard-driven (scroll still works there).
            MouseEventKind::Down(MouseButton::Left)
                if matches!(self.overlay, Overlay::None)
                    && self.detail.is_none()
                    && !self.searching =>
            {
                self.on_click(mouse.column, mouse.row);
            }
            _ => {}
        }
    }

    /// Handle a left click at `(column, row)` on the main tab view: a click on
    /// the tab bar (the panel's top border) switches tabs; a click on a table
    /// row selects it, and on the Parties tab opens that party's detail.
    fn on_click(&mut self, column: u16, row: u16) {
        let body = self.body_area;
        if body.width == 0 || body.height == 0 {
            return;
        }
        // The tab labels live on the panel's top border line.
        if row == body.y {
            if let Some(tab) = tab_at(body, column) {
                self.switch_to(tab);
            }
            return;
        }
        // Table rows: the block border + horizontal padding inset the content by
        // two columns; the header sits on the first inner line, data rows below.
        let inner_left = body.x + 2;
        let inner_right = body.x + body.width.saturating_sub(2);
        let first_row = body.y + 2;
        let last_row = body.y + body.height.saturating_sub(1); // bottom border
        if column < inner_left || column >= inner_right || row < first_row || row >= last_row {
            return;
        }
        let visible_index = (row - first_row) as usize;
        let len = self.current_len();
        let index = self.current_table_mut().offset() + visible_index;
        if index >= len {
            return;
        }
        self.current_table_mut().select(Some(index));
        if self.active_tab == Tab::Parties {
            self.open_party_detail();
        }
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
            // View this node's key status and DSO network identity.
            (KeyCode::Char('i'), _) if self.active_tab == Tab::Peers => self.open_node_info(),
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
            // Add a new member to this party.
            KeyCode::Char('P') => self.open_add_party(),
            // Change this party's signing threshold.
            KeyCode::Char('T') => self.open_change_threshold(),
            // View the on-chain governance audit trail.
            KeyCode::Char('c') => self.open_chain_audit(),
            // Open the governance approvals list (confirm / execute / revoke).
            KeyCode::Char('g') => self.open_governance(),
            // Deploy governance-core contracts for this party.
            KeyCode::Char('D') => self.open_deploy(),
            // Configure / log in this party's IdP credentials.
            KeyCode::Char('a') => self.open_party_login(),
            _ => {}
        }
    }
}
