//! Key handling inside the overlays and the composer.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::composer::{Composer, FieldKind, PickerOption, SelectOption};

use crate::app::forms::{pc_field_mut, validate_peer_form};
use crate::app::overlay::{ComposerKind, IdpMode, Overlay, PcRow, PeerForm};
use crate::app::state::App;

impl App {
    /// Handle a key while a modal overlay is open.
    pub(super) fn on_overlay_key(&mut self, key: KeyEvent) {
        /// What an overlay key resolves to, applied after the borrow ends.
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
            Overlay::PartyConfig(form) => {
                let rows = form.rows();
                let row = rows.get(form.cursor).copied();
                match key.code {
                    KeyCode::Esc => act = Act::Close,
                    KeyCode::Up => form.cursor = form.cursor.saturating_sub(1),
                    KeyCode::Down | KeyCode::Tab if form.cursor + 1 < rows.len() => {
                        form.cursor += 1;
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                        if matches!(row, Some(PcRow::Mode)) =>
                    {
                        form.mode = match form.mode {
                            IdpMode::Keycloak => IdpMode::Auth0,
                            IdpMode::Auth0 => IdpMode::Keycloak,
                        };
                        form.cursor = 0;
                    }
                    KeyCode::Enter => match row {
                        Some(PcRow::Discover) => act = Act::Discover,
                        Some(PcRow::Submit) => act = Act::SubmitPartyConfig,
                        _ => {
                            if form.cursor + 1 < rows.len() {
                                form.cursor += 1;
                            }
                        }
                    },
                    KeyCode::Char(c) => {
                        if let Some(field) = pc_field_mut(form, row) {
                            field.push(c);
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(field) = pc_field_mut(form, row) {
                            field.pop();
                        }
                    }
                    _ => {}
                }
            }
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
}
