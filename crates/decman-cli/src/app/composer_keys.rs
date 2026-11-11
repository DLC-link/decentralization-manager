//! Key handling inside the composer form.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::{
    app::overlay_keys::Act,
    composer::{Composer, FieldKind, PickerOption, SelectOption},
};

impl Composer {
    /// Handle a key in the composer form. Esc closes it, and every other key
    /// goes to [`composer_key`].
    pub(super) fn on_key(&mut self, key: KeyEvent) -> Act {
        if key.code == KeyCode::Esc {
            Act::Close
        } else if composer_key(self, key) {
            Act::SubmitComposer
        } else {
            Act::None
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
