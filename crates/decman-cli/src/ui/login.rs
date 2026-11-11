//! The login screen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Paragraph, Row, Table, TableState};

use crate::config::Profile;

use crate::ui::frame::draw_header;
use crate::ui::widgets::{dash_if_empty, header_style, highlight_style, popup_block};

/// Render the login menu: a table of profiles to choose from.
pub fn draw_login(frame: &mut Frame, profiles: &[Profile], state: &mut TableState) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // No node is connected on the profile picker, so no version to show.
    draw_header(frame, header, None);

    let header_row = Row::new(["PROFILE", "NETWORK", "USER", "API URL"]).style(header_style());
    let rows: Vec<Row> = profiles
        .iter()
        .map(|profile| {
            Row::new([
                profile.name.clone(),
                dash_if_empty(&profile.network),
                profile.username.clone(),
                profile.api_url.clone(),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(20),
        Constraint::Length(10),
        Constraint::Length(30),
        Constraint::Fill(1),
    ];
    let table = Table::new(rows, widths)
        .header(header_row)
        .block(popup_block("Select profile"))
        .column_spacing(2)
        .highlight_symbol("▶ ")
        .row_highlight_style(highlight_style());
    frame.render_stateful_widget(table, body, state);

    frame.render_widget(
        Paragraph::new(" ↑/↓ select · enter log in · q quit")
            .style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}
