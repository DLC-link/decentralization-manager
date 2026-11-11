//! Key handling inside the overlays and the composer.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::composer::{Composer, FieldKind, PickerOption, SelectOption};

use crate::app::forms::{pc_field_mut, validate_peer_form};
use crate::app::overlay::{ComposerKind, DeployForm, GrantForm, IdpMode, NetworkEditState};
use crate::app::overlay::{OnboardForm, Overlay, PartyConfigForm, PcRow, PeerForm};
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
            Overlay::Onboard(form) => act = form.on_key(key),
            Overlay::MemberChange(form) => match key.code {
                KeyCode::Esc => act = Act::Close,
                KeyCode::Enter => act = Act::MemberChange,
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
            Overlay::ChangeThreshold(form) => match key.code {
                KeyCode::Esc => act = Act::Close,
                KeyCode::Enter => act = Act::ChangeThreshold,
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
                KeyCode::Char('e') => act = Act::OpenPartyConfig,
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') if *selected + 1 < parties.len() => {
                    *selected += 1;
                }
                _ => {}
            },
            Overlay::GrantRights(form) => act = form.on_key(key),
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
            Overlay::Deploy(form) => act = form.on_key(key),
            Overlay::NetworkEdit(state) => act = state.on_key(key),
            Overlay::PartyConfig(form) => act = form.on_key(key),
            Overlay::NodeInfo(_) | Overlay::Message(_) | Overlay::Busy(_) => {
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

    use crate::api::PeerEntry;
    use crate::app::overlay::PeerChoice;
    use crate::app::worker::Request;
    use crate::composer::PickerOption;

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
}
