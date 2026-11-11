//! Shared widgets, colors and text helpers.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};

use common::types::Permission;

use crate::logo;

/// A dim gray line of plain text.
pub fn dim_line(text: &str) -> Line<'_> {
    Line::styled(text, Style::default().fg(Color::DarkGray))
}

/// Pad/truncate `value` to exactly `width` columns, left-aligned.
pub fn fixed(value: &str, width: usize) -> String {
    if value.chars().count() > width {
        truncate(value, width)
    } else {
        format!("{value:<width$}")
    }
}

/// Right-align `value` to at least `width` columns.
pub fn fixed_right(value: &str, width: usize) -> String {
    format!("{value:>width$}")
}

/// Truncate keeping the head and tail with an ellipsis in the middle.
pub fn truncate_middle(value: &str, max: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max {
        return value.to_owned();
    }
    let tail = max.saturating_sub(1) / 2;
    let head = max.saturating_sub(1) - tail;
    let head: String = chars[..head].iter().collect();
    let tail: String = chars[chars.len() - tail..].iter().collect();
    format!("{head}…{tail}")
}

/// Color for an audit entry's status.
pub fn audit_status_color(status: &str) -> Color {
    match status {
        "success" | "completed" | "executed" => Color::Green,
        "failed" | "error" => Color::Red,
        _ => Color::DarkGray,
    }
}

/// Format an epoch timestamp (seconds or millis) as `YYYY-MM-DD HH:MM` (UTC).
pub fn format_timestamp(epoch: i64) -> String {
    if epoch <= 0 {
        return "—".to_owned();
    }
    let secs = if epoch > 1_000_000_000_000 {
        epoch / 1000
    } else {
        epoch
    };
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (hour, minute) = (rem / 3600, (rem % 3600) / 60);
    // Civil date from days since the Unix epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Color for a participant permission level.
pub fn permission_color(permission: &Permission) -> Color {
    match permission {
        Permission::Submission => Color::Green,
        Permission::Confirmation => logo::ORANGE,
        _ => Color::DarkGray,
    }
}

/// A bordered, centered single-line message (empty-state placeholders).
pub fn placeholder<'a>(message: &'a str, block: Block<'a>) -> Paragraph<'a> {
    Paragraph::new(message)
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center)
        .block(block)
}

/// A centered loading message with an animated spinner prefix.
pub fn loading<'a>(spinner: &str, message: &str, block: Block<'a>) -> Paragraph<'a> {
    Paragraph::new(format!("{spinner} {message}"))
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center)
        .block(block)
}

/// A bordered error panel with a retry hint.
pub fn error_widget<'a>(message: &'a str, block: Block<'a>) -> Paragraph<'a> {
    let text = vec![
        Line::styled(
            "Failed to reach decman",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Line::default(),
        Line::raw(message),
        Line::default(),
        Line::styled(
            "Press r to retry · q to quit",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    Paragraph::new(text)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(block)
}

/// The orange selection-bar style shared by the tables.
pub fn highlight_style() -> Style {
    Style::default()
        .bg(logo::ORANGE)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

/// Style for a table header row.
pub fn header_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

/// Return the string, or an em dash when it is empty.
pub fn dash_if_empty(value: &str) -> String {
    if value.is_empty() {
        "—".to_owned()
    } else {
        value.to_owned()
    }
}

/// Truncate a string to `max` characters, appending an ellipsis when clipped.
pub fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let kept: String = value.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Compute a [`Rect`] of the given size centered within `area`, clamped to fit.
pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width - width) / 2;
    let y = area.y + (area.height - height) / 2;
    Rect::new(x, y, width, height)
}

/// Draw a one-cell drop shadow behind a popup `rect` (offset down-right and
/// clamped to `area`). Rendered before the popup's own `Clear`, so only the
/// protruding bottom row and right column remain visible as the shadow.
pub fn draw_shadow(frame: &mut Frame, rect: Rect, area: Rect) {
    let x = rect.x.saturating_add(1);
    let y = rect.y.saturating_add(1);
    let max_x = area.x.saturating_add(area.width);
    let max_y = area.y.saturating_add(area.height);
    if x >= max_x || y >= max_y {
        return;
    }
    let shadow = Rect::new(x, y, rect.width.min(max_x - x), rect.height.min(max_y - y));
    frame.render_widget(Clear, shadow);
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Rgb(18, 18, 18))),
        shadow,
    );
}

/// The bordered block used by modal popups.
pub fn popup_block(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(logo::ORANGE))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD),
        ))
}

/// A `new threshold` editor line shared by the add-party and change-threshold
/// popups: the current value in orange, then a dim hint with the old value,
/// the ceiling, and the adjust keys.
pub fn threshold_line(new_threshold: i32, previous: i32, max: i32) -> Line<'static> {
    Line::from(vec![
        Span::styled("New threshold  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{new_threshold} "),
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("(was {previous}, max {max}) ←/→ adjust"),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

#[cfg(test)]
mod tests {

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use ratatui::layout::Rect;

    use super::*;

    #[test]
    fn draw_shadow_clamps_at_screen_edge_without_panicking() {
        // `TestBackend` is infallible, so `Terminal::new` can't error here.
        let Ok(mut terminal) = Terminal::new(TestBackend::new(20, 10));
        // A popup flush against the bottom-right corner: the offset shadow must
        // clamp to the screen rather than panic on out-of-bounds.
        let drawn = terminal.draw(|frame| {
            let area = frame.area();
            draw_shadow(frame, Rect::new(18, 8, 2, 2), area);
            draw_shadow(frame, Rect::new(5, 3, 8, 4), area);
        });
        assert!(drawn.is_ok());
    }
}
