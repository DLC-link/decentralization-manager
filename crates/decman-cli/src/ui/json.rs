//! JSON lines and syntax highlighting.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Syntax-highlighted, pretty-printed JSON lines for the audit-details modal.
pub fn json_lines(value: &serde_json::Value) -> Vec<Line<'static>> {
    if value.is_null() {
        return vec![Line::styled(
            "(no details)",
            Style::default().fg(Color::DarkGray),
        )];
    }
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| value.to_string())
        .lines()
        .map(|line| Line::from(highlight_json(line)))
        .collect()
}

/// Tokenize one line of (pretty-printed) JSON into color-styled spans: keys,
/// strings, numbers, `true`/`false`/`null`, and punctuation.
fn highlight_json(line: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            // String literal (handles escapes); a key if followed by `:`.
            let start = i;
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '\\' => i += 2,
                    '"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            let text: String = chars[start..i.min(chars.len())].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            let color = if chars.get(j) == Some(&':') {
                Color::Cyan
            } else {
                Color::Green
            };
            spans.push(Span::styled(text, Style::default().fg(color)));
        } else if c.is_ascii_digit()
            || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            i += 1;
            while i < chars.len() && matches!(chars[i], '0'..='9' | '.' | 'e' | 'E' | '+' | '-') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            spans.push(Span::styled(text, Style::default().fg(Color::Yellow)));
        } else if let Some(literal) = json_literal_at(&chars, i) {
            spans.push(Span::styled(literal, Style::default().fg(Color::Magenta)));
            i += literal.len();
        } else {
            // A run of punctuation / whitespace up to the next token.
            let start = i;
            i += 1;
            while i < chars.len() && !json_token_starts(&chars, i) {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            spans.push(Span::styled(text, Style::default().fg(Color::DarkGray)));
        }
    }
    spans
}

/// The JSON keyword starting at `i`, if any.
fn json_literal_at(chars: &[char], i: usize) -> Option<&'static str> {
    ["true", "false", "null"].into_iter().find(|literal| {
        let end = i + literal.len();
        end <= chars.len() && chars[i..end].iter().copied().eq(literal.chars())
    })
}

/// Whether a JSON token (string, number, or keyword) begins at `i`.
fn json_token_starts(chars: &[char], i: usize) -> bool {
    let c = chars[i];
    c == '"'
        || c.is_ascii_digit()
        || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        || json_literal_at(chars, i).is_some()
}

#[cfg(test)]
mod tests {

    use ratatui::style::Color;

    use super::*;

    #[test]
    fn highlight_json_colors_tokens() {
        let spans = highlight_json(r#"  "key": "val", "count": 3, "ok": true, "x": null"#);
        let color_of = |text: &str| {
            spans
                .iter()
                .find(|span| span.content == text)
                .unwrap_or_else(|| panic!("no span {text:?}"))
                .style
                .fg
        };
        assert_eq!(color_of("\"key\""), Some(Color::Cyan)); // key
        assert_eq!(color_of("\"val\""), Some(Color::Green)); // string value
        assert_eq!(color_of("3"), Some(Color::Yellow)); // number
        assert_eq!(color_of("true"), Some(Color::Magenta)); // boolean
        assert_eq!(color_of("null"), Some(Color::Magenta)); // null
    }
}
