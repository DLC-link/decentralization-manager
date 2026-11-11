//! Key handling inside the overlays and the composer.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::composer::{Composer, FieldKind, PickerOption, SelectOption};

use crate::app::forms::{pc_field_mut, validate_peer_form};
use crate::app::overlay::{ChangeThresholdForm, ComposerKind, ComposerPick, DeployForm, GovView};
use crate::app::overlay::{GrantForm, IdpMode, MemberChangeForm, NetworkEditState, OnboardForm};
use crate::app::overlay::{Overlay, PartyConfigForm, PcRow, PeerChoice, PeerForm};
use crate::app::state::App;

/// What an overlay key resolves to, applied after the borrow of the overlay
/// ends.
enum Act {
    None,
    Close,
    Distribute,
    Onboard,
    MemberChange,
    ChangeThreshold,
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
    OpenPartyConfig,
    SubmitPartyConfig,
    Discover,
}

impl App {
    /// Handle a key while a modal overlay is open.
    pub(super) fn on_overlay_key(&mut self, key: KeyEvent) {
        let act = match &mut self.overlay {
            Overlay::PeerSelect { peers, cursor, .. } => peer_select_key(peers, cursor, key),
            Overlay::Compare { scroll, .. }
            | Overlay::Json { scroll, .. }
            | Overlay::FeedDetail { scroll, .. }
            | Overlay::ChainAudit { scroll, .. } => scroll_key(scroll, key),
            Overlay::Onboard(form) => form.on_key(key),
            Overlay::MemberChange(form) => form.on_key(key),
            Overlay::ChangeThreshold(form) => form.on_key(key),
            Overlay::Auth { parties, selected } => auth_key(parties.len(), selected, key),
            Overlay::GrantRights(form) => form.on_key(key),
            Overlay::Governance(view) => view.on_key(key),
            Overlay::ComposerPick(pick) => pick.on_key(key),
            Overlay::Composer(composer) => composer.on_key(key),
            Overlay::Deploy(form) => form.on_key(key),
            Overlay::NetworkEdit(state) => state.on_key(key),
            Overlay::PartyConfig(form) => form.on_key(key),
            Overlay::NodeInfo(_) | Overlay::Message(_) | Overlay::Busy(_) => dismiss_key(key),
            Overlay::None => Act::None,
        };
        self.apply_act(act);
    }

    /// Carry out what an overlay key resolved to.
    fn apply_act(&mut self, act: Act) {
        match act {
            Act::None => {}
            Act::Close => self.overlay = Overlay::None,
            Act::Distribute => self.confirm_distribute(),
            Act::Onboard => self.confirm_onboard(),
            Act::MemberChange => self.confirm_member_change(),
            Act::ChangeThreshold => self.confirm_change_threshold(),
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
            Act::OpenPartyConfig => self.open_party_config(),
            Act::SubmitPartyConfig => self.submit_party_config(),
            Act::Discover => self.run_discover(),
        }
    }
}

/// Move `cursor` one row down, stopping on the last of `rows`.
fn step_down(cursor: &mut usize, rows: usize) {
    if *cursor + 1 < rows {
        *cursor += 1;
    }
}

/// Type a character into `field`, or delete its last one on Backspace. Other
/// keys, and a row without a field, change nothing.
fn edit_text(field: Option<&mut String>, code: KeyCode) {
    let Some(field) = field else {
        return;
    };
    match code {
        KeyCode::Char(c) => field.push(c),
        KeyCode::Backspace => {
            field.pop();
        }
        _ => {}
    }
}

/// Handle a key in the peer picker of a DAR distribution.
fn peer_select_key(peers: &mut [PeerChoice], cursor: &mut usize, key: KeyEvent) -> Act {
    match key.code {
        KeyCode::Esc => return Act::Close,
        KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => step_down(cursor, peers.len()),
        KeyCode::Char(' ') => {
            if let Some(peer) = peers.get_mut(*cursor) {
                peer.checked = !peer.checked;
            }
        }
        KeyCode::Enter => return Act::Distribute,
        _ => {}
    }
    Act::None
}

/// Handle a key in a read-only overlay that scrolls.
fn scroll_key(scroll: &mut u16, key: KeyEvent) -> Act {
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return Act::Close,
        KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
        _ => {}
    }
    Act::None
}

/// Handle a key in the authentication status overlay, which lists
/// `party_count` parties.
fn auth_key(party_count: usize, selected: &mut usize, key: KeyEvent) -> Act {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return Act::Close,
        KeyCode::Char('t') => return Act::TestAuth,
        KeyCode::Char('g') => return Act::OpenGrant,
        KeyCode::Char('e') => return Act::OpenPartyConfig,
        KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => step_down(selected, party_count),
        _ => {}
    }
    Act::None
}

/// Close a message, busy or node-info overlay on Esc, Enter or q.
fn dismiss_key(key: KeyEvent) -> Act {
    if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
        Act::Close
    } else {
        Act::None
    }
}

/// Lower `threshold` with ← or - (not below 1), or raise it with → or + (not
/// above `max`). Other keys change nothing.
fn adjust_threshold(threshold: &mut i32, max: i32, code: KeyCode) {
    match code {
        KeyCode::Left | KeyCode::Char('-') => *threshold = (*threshold - 1).max(1),
        KeyCode::Right | KeyCode::Char('+') => *threshold = (*threshold + 1).min(max),
        _ => {}
    }
}

impl MemberChangeForm {
    /// Handle a key in the kick or add-party form: pick a candidate and set
    /// the new threshold.
    fn on_key(&mut self, key: KeyEvent) -> Act {
        match key.code {
            KeyCode::Esc => return Act::Close,
            KeyCode::Enter => return Act::MemberChange,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                step_down(&mut self.selected, self.candidates.len());
            }
            code => adjust_threshold(&mut self.new_threshold, self.max_threshold, code),
        }
        Act::None
    }
}

impl ChangeThresholdForm {
    /// Handle a key in the change-threshold form.
    fn on_key(&mut self, key: KeyEvent) -> Act {
        match key.code {
            KeyCode::Esc => return Act::Close,
            KeyCode::Enter => return Act::ChangeThreshold,
            code => adjust_threshold(&mut self.new_threshold, self.max_threshold, code),
        }
        Act::None
    }
}

impl GovView {
    /// Handle a key in the governance approvals list.
    fn on_key(&mut self, key: KeyEvent) -> Act {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Act::Close,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => step_down(&mut self.selected, self.items.len()),
            KeyCode::Char('c') => return Act::GovConfirm,
            KeyCode::Char('e') => return Act::GovExecute,
            KeyCode::Char('r') => return Act::GovRevoke,
            KeyCode::Char('x') => return Act::GovExpire,
            KeyCode::Char('n') => return Act::ComposeAction,
            KeyCode::Char('p') => return Act::ComposeProposal,
            _ => {}
        }
        Act::None
    }
}

impl ComposerPick {
    /// Handle a key in the action or proposal type picker.
    fn on_key(&mut self, key: KeyEvent) -> Act {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Act::Close,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => step_down(&mut self.selected, self.options.len()),
            KeyCode::Enter => return Act::OpenComposer,
            _ => {}
        }
        Act::None
    }
}

impl Composer {
    /// Handle a key in the composer form. Esc closes it, and every other key
    /// goes to [`composer_key`].
    fn on_key(&mut self, key: KeyEvent) -> Act {
        if key.code == KeyCode::Esc {
            Act::Close
        } else if composer_key(self, key) {
            Act::SubmitComposer
        } else {
            Act::None
        }
    }
}

impl OnboardForm {
    /// Handle a key in the onboarding form: the prefix field, then one row
    /// per peer.
    fn on_key(&mut self, key: KeyEvent) -> Act {
        match key.code {
            KeyCode::Esc => return Act::Close,
            KeyCode::Enter => return Act::Onboard,
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => step_down(&mut self.cursor, self.peers.len() + 1),
            KeyCode::Char(' ') if !self.on_prefix() => {
                if let Some(peer) = self.peer_index().and_then(|i| self.peers.get_mut(i)) {
                    peer.checked = !peer.checked;
                }
            }
            KeyCode::Backspace if self.on_prefix() => {
                self.prefix.pop();
            }
            KeyCode::Char(c) if self.on_prefix() => self.prefix.push(c),
            _ => {}
        }
        Act::None
    }
}

impl GrantForm {
    /// The text field under the cursor. The submit row has none.
    fn field_mut(&mut self) -> Option<&mut String> {
        match self.cursor {
            0 => Some(&mut self.client_id),
            1 => Some(&mut self.client_secret),
            _ => None,
        }
    }

    /// Handle a key in the grant-rights form: two fields and a submit row.
    fn on_key(&mut self, key: KeyEvent) -> Act {
        match key.code {
            KeyCode::Esc => return Act::Close,
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Enter if self.cursor >= 2 => return Act::SubmitGrant,
            // Enter on a field moves on, like Down.
            KeyCode::Down | KeyCode::Tab | KeyCode::Enter => step_down(&mut self.cursor, 3),
            KeyCode::Char(_) | KeyCode::Backspace => edit_text(self.field_mut(), key.code),
            _ => {}
        }
        Act::None
    }
}

impl DeployForm {
    /// The text field under the cursor. The submit row has none.
    fn field_mut(&mut self) -> Option<&mut String> {
        match self.cursor {
            0 => Some(&mut self.threshold),
            1 => Some(&mut self.timeout_micros),
            _ => None,
        }
    }

    /// Handle a key in the deploy form: two number fields and a submit row.
    fn on_key(&mut self, key: KeyEvent) -> Act {
        match key.code {
            KeyCode::Esc => return Act::Close,
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Enter if self.cursor >= 2 => return Act::SubmitDeploy,
            // Enter on a field moves on, like Down.
            KeyCode::Down | KeyCode::Tab | KeyCode::Enter => step_down(&mut self.cursor, 3),
            KeyCode::Char(c) if c.is_ascii_digit() => edit_text(self.field_mut(), key.code),
            KeyCode::Backspace => edit_text(self.field_mut(), key.code),
            _ => {}
        }
        Act::None
    }
}

impl PeerForm {
    /// The cursor row of the port field, which takes digits only.
    const PORT_ROW: usize = 3;

    /// The text field under the cursor. The submit row has none.
    fn field_mut(&mut self) -> Option<&mut String> {
        match self.cursor {
            0 => Some(&mut self.participant_id),
            1 => Some(&mut self.name),
            2 => Some(&mut self.address),
            3 => Some(&mut self.port),
            4 => Some(&mut self.public_key),
            5 => Some(&mut self.party),
            _ => None,
        }
    }
}

impl NetworkEditState {
    /// Handle a key in the network editor: the add-peer form while it is
    /// open, the peer list otherwise.
    fn on_key(&mut self, key: KeyEvent) -> Act {
        match &mut self.adding {
            Some(form) => match key.code {
                KeyCode::Esc => self.adding = None,
                KeyCode::Up => form.cursor = form.cursor.saturating_sub(1),
                KeyCode::Enter if form.cursor >= 6 => match validate_peer_form(form) {
                    // Only add a valid peer; otherwise land the cursor on
                    // the first invalid field rather than writing port 0.
                    Ok(peer) => {
                        self.peers.push(peer);
                        self.adding = None;
                    }
                    Err(invalid_field) => form.cursor = invalid_field,
                },
                // Enter on a field moves on, like Down.
                KeyCode::Down | KeyCode::Tab | KeyCode::Enter => step_down(&mut form.cursor, 7),
                KeyCode::Char(c) if form.cursor != PeerForm::PORT_ROW || c.is_ascii_digit() => {
                    edit_text(form.field_mut(), key.code);
                }
                KeyCode::Backspace => edit_text(form.field_mut(), key.code),
                _ => {}
            },
            None => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Act::Close,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    step_down(&mut self.selected, self.peers.len());
                }
                KeyCode::Char('a') => self.adding = Some(PeerForm::blank()),
                KeyCode::Char('d') if self.selected < self.peers.len() => {
                    self.peers.remove(self.selected);
                    self.selected = self.selected.min(self.peers.len().saturating_sub(1));
                }
                KeyCode::Char('s') => return Act::SaveNetwork,
                _ => {}
            },
        }
        Act::None
    }
}

impl PartyConfigForm {
    /// Handle a key in the party-config form. The rows depend on the mode.
    fn on_key(&mut self, key: KeyEvent) -> Act {
        let rows = self.rows();
        let row = rows.get(self.cursor).copied();
        match key.code {
            KeyCode::Esc => return Act::Close,
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => step_down(&mut self.cursor, rows.len()),
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if matches!(row, Some(PcRow::Mode)) =>
            {
                self.mode = match self.mode {
                    IdpMode::Keycloak => IdpMode::Auth0,
                    IdpMode::Auth0 => IdpMode::Keycloak,
                };
                self.cursor = 0;
            }
            KeyCode::Enter => match row {
                Some(PcRow::Discover) => return Act::Discover,
                Some(PcRow::Submit) => return Act::SubmitPartyConfig,
                _ => step_down(&mut self.cursor, rows.len()),
            },
            KeyCode::Char(_) | KeyCode::Backspace => edit_text(pc_field_mut(self, row), key.code),
            _ => {}
        }
        Act::None
    }
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

/// The picker option value one step forward / back from `current`. When
/// `current` matches no option (nothing picked yet, or a typed cid), the first
/// press lands on the first option.
fn next_picker_value(options: &[PickerOption], current: &str, forward: bool) -> String {
    match options.iter().position(|option| option.value == current) {
        None => options
            .first()
            .map_or_else(|| current.to_owned(), |o| o.value.clone()),
        Some(index) => {
            let len = options.len();
            let next = if forward {
                (index + 1) % len
            } else {
                (index + len - 1) % len
            };
            options[next].value.clone()
        }
    }
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
            // ← / → cycle the fetched options; typing / backspace stay available
            // so a cid can be pasted while the list loads or when it is empty.
            FieldKind::Picker {
                options, loaded, ..
            } => match key.code {
                KeyCode::Left if *loaded && !options.is_empty() => {
                    field.value = next_picker_value(options, &field.value, false);
                }
                KeyCode::Right if *loaded && !options.is_empty() => {
                    field.value = next_picker_value(options, &field.value, true);
                }
                KeyCode::Char(c) => field.value.push(c),
                KeyCode::Backspace => {
                    field.value.pop();
                }
                _ => {}
            },
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use crate::api::{AuthStatusKind, PartyAuthStatus, PeerEntry};
    use crate::app::overlay::{Dar, GovItem, GovOp};
    use crate::app::worker::Request;
    use crate::composer::PickerOption;
    use crate::composer::{ComposerField, ComposerSubmit};

    use super::*;

    #[test]
    fn next_picker_value_cycles_and_starts_at_first() {
        let options = vec![
            PickerOption {
                value: "a".to_owned(),
                label: "A".to_owned(),
            },
            PickerOption {
                value: "b".to_owned(),
                label: "B".to_owned(),
            },
            PickerOption {
                value: "c".to_owned(),
                label: "C".to_owned(),
            },
        ];
        // Nothing (or a typed cid) selected → first press lands on the first.
        assert_eq!(next_picker_value(&options, "", true), "a");
        assert_eq!(next_picker_value(&options, "", false), "a");
        assert_eq!(next_picker_value(&options, "typed", true), "a");
        // Forward and backward wrap around the ends.
        assert_eq!(next_picker_value(&options, "c", true), "a");
        assert_eq!(next_picker_value(&options, "a", false), "c");
        assert_eq!(next_picker_value(&options, "a", true), "b");
        // No options → the current value is kept.
        assert_eq!(next_picker_value(&[], "x", true), "x");
    }

    fn app_with(overlay: Overlay) -> (App, Receiver<Request>) {
        let (requests, request_rx) = mpsc::channel();
        let (_updates_tx, updates) = mpsc::channel();
        let mut app = App::new(requests, updates, false);
        app.overlay = overlay;
        (app, request_rx)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_overlay_key(KeyEvent::from(code));
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn busy_text(app: &App) -> &str {
        match &app.overlay {
            Overlay::Busy(text) => text,
            _ => panic!("expected a busy overlay"),
        }
    }

    fn assert_esc_closes(overlay: Overlay) {
        let (mut app, requests) = app_with(overlay);
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.overlay, Overlay::None));
        assert!(requests.try_recv().is_err());
    }

    fn onboard_form() -> Overlay {
        let peer = |id: &str, checked| PeerChoice {
            id: id.to_owned(),
            name: id.to_owned(),
            checked,
        };
        Overlay::Onboard(OnboardForm {
            prefix: String::new(),
            peers: vec![peer("a", true), peer("b", false)],
            cursor: 0,
        })
    }

    fn onboard(app: &App) -> &OnboardForm {
        match &app.overlay {
            Overlay::Onboard(form) => form,
            _ => panic!("expected the onboard form"),
        }
    }

    #[test]
    fn onboard_form_keys() {
        let (mut app, requests) = app_with(onboard_form());
        type_text(&mut app, "tr x");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(onboard(&app).prefix, "tr");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        assert_eq!(onboard(&app).cursor, 2, "the cursor stops on the last peer");
        press(&mut app, KeyCode::Char(' '));
        type_text(&mut app, "zk");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(onboard(&app).prefix, "tr", "a peer row takes no text");
        assert_eq!(onboard(&app).cursor, 2, "k does not move the cursor");
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char(' '));
        let checked: Vec<bool> = onboard(&app).peers.iter().map(|p| p.checked).collect();
        assert_eq!(checked, vec![false, true]);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        assert_eq!(onboard(&app).cursor, 0);
        press(&mut app, KeyCode::Left);
        assert_eq!(onboard(&app).prefix, "tr");
        press(&mut app, KeyCode::Enter);
        let Ok(Request::Onboard { prefix, peer_ids }) = requests.try_recv() else {
            panic!("expected an onboard request");
        };
        assert_eq!((prefix.as_str(), peer_ids), ("tr", vec!["b".to_owned()]));
        assert_eq!(busy_text(&app), "Starting onboarding for tr…");
        assert_esc_closes(onboard_form());
    }

    fn grant_form() -> Overlay {
        Overlay::GrantRights(Box::new(GrantForm {
            dec_party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            client_id: String::new(),
            client_secret: String::new(),
            cursor: 0,
        }))
    }

    fn grant(app: &App) -> &GrantForm {
        match &app.overlay {
            Overlay::GrantRights(form) => form,
            _ => panic!("expected the grant form"),
        }
    }

    #[test]
    fn grant_form_keys() {
        let (mut app, requests) = app_with(grant_form());
        type_text(&mut app, "admin x");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "s3j");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(
            (
                grant(&app).client_id.as_str(),
                grant(&app).client_secret.as_str()
            ),
            ("admin", "s3")
        );
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        assert_eq!(grant(&app).cursor, 2, "the cursor stops on the submit row");
        type_text(&mut app, "x");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(
            grant(&app).client_secret,
            "s3",
            "the submit row takes no text"
        );
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        assert_eq!(grant(&app).cursor, 0);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(requests.try_recv().is_err(), "Enter on a field moves on");
        press(&mut app, KeyCode::Enter);
        let Ok(Request::GrantRights {
            dec_party_id,
            client_id,
            client_secret,
        }) = requests.try_recv()
        else {
            panic!("expected a grant request");
        };
        assert_eq!(
            (
                dec_party_id.as_str(),
                client_id.as_str(),
                client_secret.as_str()
            ),
            ("dec::1220", "admin", "s3")
        );
        assert_eq!(busy_text(&app), "Granting rights…");
        assert_esc_closes(grant_form());
    }

    fn deploy_form() -> Overlay {
        Overlay::Deploy(Box::new(DeployForm {
            party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            package_id: "#governance-core-v1".to_owned(),
            operator_party: "op::1220".to_owned(),
            participant_ids: vec!["p1::1220".to_owned(), "p2::1220".to_owned()],
            member_parties: vec!["m1::1220".to_owned(), "m2::1220".to_owned()],
            threshold: "2".to_owned(),
            timeout_micros: "86400000000".to_owned(),
            cursor: 0,
        }))
    }

    fn deploy(app: &App) -> &DeployForm {
        match &app.overlay {
            Overlay::Deploy(form) => form,
            _ => panic!("expected the deploy form"),
        }
    }

    #[test]
    fn deploy_form_keys() {
        let (mut app, requests) = app_with(deploy_form());
        type_text(&mut app, "a1 ");
        assert_eq!(deploy(&app).threshold, "21", "only digits are typed");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Backspace);
        type_text(&mut app, "5");
        assert_eq!(
            (
                deploy(&app).threshold.as_str(),
                deploy(&app).timeout_micros.as_str()
            ),
            ("2", "86400000005")
        );
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Tab);
        assert_eq!(deploy(&app).cursor, 2, "the cursor stops on the submit row");
        type_text(&mut app, "7");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(deploy(&app).timeout_micros, "86400000005");
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        assert_eq!(deploy(&app).cursor, 0);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        assert!(requests.try_recv().is_err(), "Enter on a field moves on");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(requests.try_recv(), Ok(Request::DeploySubmit(_))));
        assert_eq!(busy_text(&app), "Deploying contracts…");
        assert_esc_closes(deploy_form());
    }

    fn network_edit(adding: bool) -> Overlay {
        let existing = PeerEntry {
            participant_id: "old::1220".to_owned(),
            name: "old".to_owned(),
            address: "10.0.0.9".to_owned(),
            port: 9000,
            public_key: "key-old".to_owned(),
            party: None,
        };
        Overlay::NetworkEdit(Box::new(NetworkEditState {
            peers: vec![existing],
            selected: 0,
            adding: adding.then(PeerForm::blank),
        }))
    }

    fn network(app: &App) -> &NetworkEditState {
        match &app.overlay {
            Overlay::NetworkEdit(state) => state,
            _ => panic!("expected the network editor"),
        }
    }

    fn peer_form(app: &App) -> &PeerForm {
        match &network(app).adding {
            Some(form) => form,
            None => panic!("expected the add-peer form"),
        }
    }

    #[test]
    fn add_peer_form_keys() {
        let (mut app, requests) = app_with(network_edit(true));
        type_text(&mut app, "new::1220");
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "New");
        press(&mut app, KeyCode::Tab);
        type_text(&mut app, "10.0.0.2");
        press(&mut app, KeyCode::Down);
        type_text(&mut app, "90a01");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(peer_form(&app).port, "900", "the port takes only digits");
        type_text(&mut app, "1");
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "key-new");
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "party::1220x");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(
            peer_form(&app).cursor,
            6,
            "the cursor stops on the submit row"
        );
        type_text(&mut app, "z");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(peer_form(&app).party, "party::1220");

        // An invalid peer moves the cursor to the first bad field.
        let saved = peer_form(&app).address.clone();
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        for _ in 0..saved.len() {
            press(&mut app, KeyCode::Backspace);
        }
        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(peer_form(&app).cursor, 2);
        type_text(&mut app, &saved);
        for _ in 0..4 {
            press(&mut app, KeyCode::Tab);
        }
        press(&mut app, KeyCode::Enter);
        assert!(network(&app).adding.is_none());
        let peers: Vec<(&str, u16, Option<&str>)> = network(&app)
            .peers
            .iter()
            .map(|p| (p.participant_id.as_str(), p.port, p.party.as_deref()))
            .collect();
        assert_eq!(
            peers,
            vec![
                ("old::1220", 9000, None),
                ("new::1220", 9001, Some("party::1220"))
            ]
        );
        assert!(
            requests.try_recv().is_err(),
            "adding a peer saves nothing yet"
        );

        // Esc leaves the add-peer form, not the editor.
        let (mut app, _requests) = app_with(network_edit(true));
        press(&mut app, KeyCode::Up);
        assert_eq!(peer_form(&app).cursor, 0);
        press(&mut app, KeyCode::Esc);
        assert!(network(&app).adding.is_none());
        assert_eq!(network(&app).peers.len(), 1);
    }

    fn party_config_form() -> Overlay {
        Overlay::PartyConfig(Box::new(PartyConfigForm {
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
        }))
    }

    fn party_config(app: &App) -> &PartyConfigForm {
        match &app.overlay {
            Overlay::PartyConfig(form) => form,
            _ => panic!("expected the party config form"),
        }
    }

    #[test]
    fn party_config_form_keys() {
        let (mut app, requests) = app_with(party_config_form());
        let mode = |app: &App| party_config(app).mode;
        press(&mut app, KeyCode::Right);
        assert!(mode(&app) == IdpMode::Auth0);
        press(&mut app, KeyCode::Char(' '));
        assert!(mode(&app) == IdpMode::Keycloak);
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Left);
        assert!(mode(&app) == IdpMode::Auth0, "only the mode row toggles");
        assert_eq!(party_config(&app).cursor, 2);

        // Toggling the mode returns the cursor to the mode row.
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Right);
        assert!(mode(&app) == IdpMode::Keycloak);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Right);
        assert!(mode(&app) == IdpMode::Keycloak);
        assert_eq!(party_config(&app).cursor, 2);

        press(&mut app, KeyCode::Up);
        type_text(&mut app, "member::1220x");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "user 1");
        press(&mut app, KeyCode::Tab);
        type_text(&mut app, "https://kc");
        assert_eq!(party_config(&app).member_party_id, "member::1220");
        assert_eq!(
            party_config(&app).user_id,
            "user 1",
            "space types on a field"
        );
        assert_eq!(party_config(&app).kc_url, "https://kc");

        // Keycloak shows nine rows: Discover is row 7, Submit row 8.
        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
        }
        assert_eq!(party_config(&app).cursor, 7);
        type_text(&mut app, "q");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(requests.try_recv(), Ok(Request::Discover(_))));
        assert_eq!(party_config(&app).status, "Discovering member party…");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Tab);
        assert_eq!(party_config(&app).cursor, 8, "the cursor stops on Submit");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            requests.try_recv(),
            Ok(Request::SavePartyConfig(_))
        ));
        assert_eq!(busy_text(&app), "Saving party configuration…");
        assert_esc_closes(party_config_form());
    }

    #[test]
    fn party_config_enter_moves_down_to_the_last_row() {
        let (mut app, requests) = app_with(party_config_form());
        for _ in 0..7 {
            press(&mut app, KeyCode::Enter);
        }
        assert_eq!(party_config(&app).cursor, 7);
        assert!(requests.try_recv().is_err());
    }

    fn change_threshold_form() -> Overlay {
        Overlay::ChangeThreshold(ChangeThresholdForm {
            party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            previous_threshold: 2,
            new_threshold: 2,
            max_threshold: 3,
        })
    }

    fn new_threshold(app: &App) -> i32 {
        match &app.overlay {
            Overlay::ChangeThreshold(form) => form.new_threshold,
            _ => panic!("expected the change-threshold form"),
        }
    }

    #[test]
    fn change_threshold_form_keys() {
        let (mut app, requests) = app_with(change_threshold_form());
        press(&mut app, KeyCode::Enter);
        assert!(
            requests.try_recv().is_err(),
            "an unchanged threshold is not sent"
        );
        assert!(
            matches!(&app.overlay, Overlay::Message(text) if text == "Threshold is already 2.")
        );

        let (mut app, requests) = app_with(change_threshold_form());
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Char('+'));
        assert_eq!(new_threshold(&app), 3, "the threshold stops at the maximum");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Char('-'));
        press(&mut app, KeyCode::Char('-'));
        assert_eq!(new_threshold(&app), 1, "the threshold stops at 1");
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Char('j'),
            KeyCode::Char('q'),
        ] {
            press(&mut app, code);
        }
        assert_eq!(new_threshold(&app), 1, "other keys do nothing");
        press(&mut app, KeyCode::Enter);
        let Ok(Request::ChangeThreshold {
            party_id,
            new_threshold,
            previous_threshold,
        }) = requests.try_recv()
        else {
            panic!("expected a change-threshold request");
        };
        assert_eq!(
            (party_id.as_str(), new_threshold, previous_threshold),
            ("dec::1220", 1, 2)
        );
        assert_eq!(busy_text(&app), "Changing threshold…");
        assert_esc_closes(change_threshold_form());
    }

    fn peer_select() -> Overlay {
        let peer = |id: &str| PeerChoice {
            id: id.to_owned(),
            name: id.to_owned(),
            checked: false,
        };
        Overlay::PeerSelect {
            dar: Dar {
                filename: "gov.dar".to_owned(),
                data: "ZGFy".to_owned(),
            },
            peers: vec![peer("a"), peer("b")],
            cursor: 0,
        }
    }

    fn peer_select_state(app: &App) -> (usize, Vec<bool>) {
        match &app.overlay {
            Overlay::PeerSelect { peers, cursor, .. } => {
                (*cursor, peers.iter().map(|p| p.checked).collect())
            }
            _ => panic!("expected the peer picker"),
        }
    }

    #[test]
    fn peer_select_keys() {
        let (mut app, requests) = app_with(peer_select());
        press(&mut app, KeyCode::Enter);
        assert!(requests.try_recv().is_err(), "no peer is ticked");
        assert_eq!(peer_select_state(&app), (0, vec![false, false]));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            peer_select_state(&app).0,
            1,
            "the cursor stops on the last peer"
        );
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Tab);
        assert_eq!(peer_select_state(&app), (0, vec![false, true]));
        press(&mut app, KeyCode::Enter);
        let Ok(Request::Distribute {
            filename,
            data,
            peer_ids,
        }) = requests.try_recv()
        else {
            panic!("expected a distribute request");
        };
        assert_eq!(
            (filename.as_str(), data.as_str(), peer_ids),
            ("gov.dar", "ZGFy", vec!["b".to_owned()])
        );
        assert_eq!(busy_text(&app), "Distributing DAR…");
        assert_esc_closes(peer_select());
    }

    #[test]
    fn scroll_overlay_keys() {
        let scroll_of = |app: &App| match &app.overlay {
            Overlay::Compare { scroll, .. }
            | Overlay::Json { scroll, .. }
            | Overlay::ChainAudit { scroll, .. } => *scroll,
            _ => panic!("expected a scrolling overlay"),
        };
        let overlays: [fn() -> Overlay; 3] = [
            || Overlay::Compare {
                comparison: common::types::PeerPackageComparison {
                    local_packages: Vec::new(),
                    peers: Vec::new(),
                },
                scroll: 0,
            },
            || Overlay::Json {
                value: serde_json::Value::Null,
                scroll: 0,
            },
            || Overlay::ChainAudit {
                entries: Vec::new(),
                scroll: 0,
            },
        ];
        for overlay in overlays {
            let (mut app, _requests) = app_with(overlay());
            press(&mut app, KeyCode::Up);
            assert_eq!(scroll_of(&app), 0);
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Char('j'));
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Char('k'));
            press(&mut app, KeyCode::Char('x'));
            assert_eq!(scroll_of(&app), 2);
            for close in [KeyCode::Esc, KeyCode::Enter, KeyCode::Char('q')] {
                let (mut app, _requests) = app_with(overlay());
                press(&mut app, close);
                assert!(matches!(app.overlay, Overlay::None));
            }
        }
    }

    fn auth_overlay() -> Overlay {
        let party = |id: &str| PartyAuthStatus {
            dec_party_id: id.to_owned(),
            member_party_id: None,
            user_id: None,
            status: AuthStatusKind::Notconfigured,
            rights: None,
        };
        Overlay::Auth {
            parties: vec![party("one::1220"), party("two::1220")],
            selected: 0,
        }
    }

    fn auth_selected(app: &App) -> usize {
        match &app.overlay {
            Overlay::Auth { selected, .. } => *selected,
            _ => panic!("expected the auth overlay"),
        }
    }

    #[test]
    fn auth_overlay_keys() {
        let (mut app, requests) = app_with(auth_overlay());
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            auth_selected(&app),
            1,
            "the selection stops on the last party"
        );
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert_eq!(auth_selected(&app), 0);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(requests.try_recv(), Ok(Request::PartyConfig(id)) if id == "two::1220"));
        assert_eq!(busy_text(&app), "Loading party configuration…");

        let (mut app, requests) = app_with(auth_overlay());
        press(&mut app, KeyCode::Char('t'));
        assert!(matches!(requests.try_recv(), Ok(Request::TestAuth)));
        assert_eq!(busy_text(&app), "Testing authentication…");

        let (mut app, requests) = app_with(auth_overlay());
        press(&mut app, KeyCode::Char('g'));
        assert!(requests.try_recv().is_err());
        assert!(
            matches!(&app.overlay, Overlay::GrantRights(form) if form.dec_party_id == "one::1220")
        );

        assert_esc_closes(auth_overlay());
        let (mut app, _requests) = app_with(auth_overlay());
        press(&mut app, KeyCode::Char('q'));
        assert!(matches!(app.overlay, Overlay::None));
    }

    fn gov_overlay(items: usize) -> Overlay {
        let item = || {
            GovItem::Domain(crate::api::DomainGovAction {
                proposal_cid: "00prop".to_owned(),
                action_label: "Mint".to_owned(),
                description: None,
                confirmations: Vec::new(),
                executable_confirmation_cids: None,
                confirmation_count: 0,
                can_execute: false,
                orphaned: false,
            })
        };
        Overlay::Governance(Box::new(GovView {
            party_name: "dec".to_owned(),
            party_id: "dec::1220".to_owned(),
            rules_contract_id: "00rules".to_owned(),
            member_party_id: "me::1220".to_owned(),
            threshold: 2,
            items: (0..items).map(|_| item()).collect(),
            selected: 0,
        }))
    }

    fn gov_selected(app: &App) -> usize {
        match &app.overlay {
            Overlay::Governance(view) => view.selected,
            _ => panic!("expected the approvals overlay"),
        }
    }

    #[test]
    fn governance_overlay_keys() {
        let (mut app, requests) = app_with(gov_overlay(2));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            gov_selected(&app),
            1,
            "the selection stops on the last item"
        );
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert_eq!(gov_selected(&app), 0);
        assert!(requests.try_recv().is_err());

        let sends = |code, busy: &str| {
            let (mut app, requests) = app_with(gov_overlay(1));
            press(&mut app, KeyCode::Char(code));
            assert!(
                matches!(requests.try_recv(), Ok(Request::GovAction(_))),
                "{code}"
            );
            assert_eq!(busy_text(&app), busy);
        };
        sends('c', "Confirming action…");
        let (mut app, _requests) = app_with(gov_overlay(1));
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(&app.overlay, Overlay::Message(text) if text.starts_with("Not enough")));
        let (mut app, _requests) = app_with(gov_overlay(1));
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(&app.overlay, Overlay::Message(text) if text.starts_with("You have no")));
        let (mut app, _requests) = app_with(gov_overlay(1));
        press(&mut app, KeyCode::Char('x'));
        assert!(matches!(&app.overlay, Overlay::Message(text) if text.starts_with("No expired")));

        for (code, kind) in [('n', "action"), ('p', "proposal")] {
            let (mut app, _requests) = app_with(gov_overlay(1));
            press(&mut app, KeyCode::Char(code));
            let Overlay::ComposerPick(pick) = &app.overlay else {
                panic!("{code} opens the type picker");
            };
            let is_action = matches!(pick.kind, ComposerKind::Action);
            assert_eq!(is_action, kind == "action");
            assert_eq!(pick.party_id, "dec::1220");
        }

        assert_esc_closes(gov_overlay(1));
        let (mut app, _requests) = app_with(gov_overlay(1));
        press(&mut app, KeyCode::Char('q'));
        assert!(matches!(app.overlay, Overlay::None));
    }

    fn composer_pick() -> Overlay {
        Overlay::ComposerPick(Box::new(ComposerPick {
            kind: ComposerKind::Proposal,
            party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            rules_contract_id: "00rules".to_owned(),
            default_threshold: 2,
            options: crate::composer::proposal_types()
                .into_iter()
                .take(2)
                .collect(),
            selected: 0,
        }))
    }

    fn pick_selected(app: &App) -> usize {
        match &app.overlay {
            Overlay::ComposerPick(pick) => pick.selected,
            _ => panic!("expected the type picker"),
        }
    }

    #[test]
    fn composer_pick_keys() {
        let (mut app, _requests) = app_with(composer_pick());
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(
            pick_selected(&app),
            1,
            "the selection stops on the last type"
        );
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(pick_selected(&app), 1, "k does not move the picker");
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(pick_selected(&app), 0, "j does not move the picker");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        let Overlay::Composer(composer) = &app.overlay else {
            panic!("Enter opens the composer");
        };
        assert_eq!(composer.action_type, "setup_cc_preapproval");

        assert_esc_closes(composer_pick());
        let (mut app, _requests) = app_with(composer_pick());
        press(&mut app, KeyCode::Char('q'));
        assert!(matches!(app.overlay, Overlay::None));
    }

    fn composer() -> Overlay {
        Overlay::Composer(Box::new(Composer {
            title: "Vote".to_owned(),
            action_type: "generic_vote",
            submit: ComposerSubmit::Propose,
            party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            rules_contract_id: "00rules".to_owned(),
            fields: vec![ComposerField {
                key: "description",
                label: "Description",
                kind: FieldKind::Text,
                value: String::new(),
                optional: false,
                help: "",
            }],
            cursor: 0,
        }))
    }

    #[test]
    fn composer_keys() {
        let (mut app, requests) = app_with(composer());
        type_text(&mut app, "hi q");
        let Overlay::Composer(form) = &app.overlay else {
            panic!("typing keeps the composer open");
        };
        assert_eq!(form.fields[0].value, "hi q");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        let Ok(Request::GovAction(request)) = requests.try_recv() else {
            panic!("Enter on the submit row proposes");
        };
        assert!(matches!(
            &request.op,
            GovOp::Propose { proposal } if proposal["description"] == "hi q"
        ));
        assert_eq!(busy_text(&app), "Submitting…");
        assert_esc_closes(composer());
    }

    #[test]
    fn message_overlays_close_on_esc_enter_or_q() {
        let overlays: [fn() -> Overlay; 2] = [
            || Overlay::Message("done".to_owned()),
            || Overlay::Busy("working".to_owned()),
        ];
        for overlay in overlays {
            for close in [KeyCode::Esc, KeyCode::Enter, KeyCode::Char('q')] {
                let (mut app, _requests) = app_with(overlay());
                press(&mut app, close);
                assert!(matches!(app.overlay, Overlay::None));
            }
            let (mut app, _requests) = app_with(overlay());
            press(&mut app, KeyCode::Char('x'));
            press(&mut app, KeyCode::Down);
            assert!(!matches!(app.overlay, Overlay::None));
        }
        let (mut app, requests) = app_with(Overlay::None);
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.overlay, Overlay::None));
        assert!(requests.try_recv().is_err());
    }
}
