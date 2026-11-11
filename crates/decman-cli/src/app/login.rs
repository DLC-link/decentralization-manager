//! The login screen loop.

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::MouseEventKind;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use crate::config::Profile;
use crate::ui;

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

        let select_next = |state: &mut TableState| {
            let next = state.selected().map_or(0, |i| (i + 1) % profiles.len());
            state.select(Some(next));
        };
        let select_previous = |state: &mut TableState| {
            let len = profiles.len();
            let previous = state.selected().map_or(0, |i| (i + len - 1) % len);
            state.select(Some(previous));
        };

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match (key.code, key.modifiers) {
                (KeyCode::Char('q') | KeyCode::Esc, _) => return Ok(None),
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(None),
                (KeyCode::Down | KeyCode::Char('j'), _) => select_next(&mut state),
                (KeyCode::Up | KeyCode::Char('k'), _) => select_previous(&mut state),
                (KeyCode::Enter, _) => {
                    return Ok(profiles.get(state.selected().unwrap_or(0)).cloned());
                }
                _ => {}
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => select_next(&mut state),
                MouseEventKind::ScrollUp => select_previous(&mut state),
                _ => {}
            },
            _ => {}
        }
    }
}
