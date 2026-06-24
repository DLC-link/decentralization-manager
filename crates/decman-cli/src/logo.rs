use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The BitSafe brand orange (`#ff6633`).
pub const ORANGE: Color = Color::Rgb(255, 102, 51);

/// Number of text rows in the rendered wordmark.
const LOGO_HEIGHT: usize = 5;

/// Block-art wordmark rendered by the TUI.
const WORDMARK: &str = "BITSAFE";

/// Render the BitSafe wordmark as styled, block-art [`Line`]s in brand orange.
///
/// Each line has identical width so the banner stays rectangular when centered.
pub fn lines() -> Vec<Line<'static>> {
    let style = Style::default().fg(ORANGE).add_modifier(Modifier::BOLD);

    (0..LOGO_HEIGHT)
        .map(|row| {
            let text = WORDMARK
                .chars()
                .map(|letter| glyph(letter)[row])
                .collect::<Vec<_>>()
                .join(" ");
            Line::from(Span::styled(text, style))
        })
        .collect()
}

/// Return the 5-row, 5-column block-art glyph for a wordmark letter.
///
/// Only the letters in [`WORDMARK`] are defined; any other character renders as
/// blank space so the banner never panics on unexpected input.
fn glyph(letter: char) -> [&'static str; LOGO_HEIGHT] {
    match letter {
        'B' => ["████ ", "█   █", "████ ", "█   █", "████ "],
        'I' => ["█████", "  █  ", "  █  ", "  █  ", "█████"],
        'T' => ["█████", "  █  ", "  █  ", "  █  ", "  █  "],
        'S' => ["█████", "█    ", "█████", "    █", "█████"],
        'A' => ["█████", "█   █", "█████", "█   █", "█   █"],
        'F' => ["█████", "█    ", "████ ", "█    ", "█    "],
        'E' => ["█████", "█    ", "████ ", "█    ", "█████"],
        _ => ["     ", "     ", "     ", "     ", "     "],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_have_uniform_nonzero_width() {
        // Arrange / Act
        let lines = lines();

        // Assert
        assert_eq!(lines.len(), LOGO_HEIGHT);
        let widths: Vec<usize> = lines.iter().map(Line::width).collect();
        assert!(widths[0] > 0);
        assert!(
            widths.iter().all(|width| *width == widths[0]),
            "all logo rows must share the same width, got {widths:?}"
        );
    }

    #[test]
    fn every_wordmark_glyph_is_square() {
        for letter in WORDMARK.chars() {
            // Act
            let rows = glyph(letter);

            // Assert
            assert_eq!(rows.len(), LOGO_HEIGHT);
            assert!(
                rows.iter().all(|row| row.chars().count() == LOGO_HEIGHT),
                "glyph '{letter}' must be {LOGO_HEIGHT} columns wide on every row"
            );
        }
    }
}
