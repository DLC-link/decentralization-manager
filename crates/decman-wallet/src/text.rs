//! Text a host chooses, made safe to put in an error.

/// The most characters of a host's text that an error keeps.
pub(crate) const MAX_ERROR_MESSAGE_CHARS: usize = 1024;

/// `message` with each control or format character replaced by a space,
/// and cut at [`MAX_ERROR_MESSAGE_CHARS`] characters.
///
/// The host chooses the text, and the error reaches logs and terminals. A
/// newline could forge a log line, and an escape sequence could drive a
/// terminal. A text of megabytes would flood both.
pub(crate) fn bounded(message: &str) -> String {
    let mut kept: String = message
        .chars()
        .take(MAX_ERROR_MESSAGE_CHARS)
        .map(|c| if is_unprintable(c) { ' ' } else { c })
        .collect();
    let more = message
        .chars()
        .count()
        .saturating_sub(MAX_ERROR_MESSAGE_CHARS);
    if more > 0 {
        kept.push_str(&format!(" ... ({more} more characters)"));
    }
    kept
}

/// A control character, a line or paragraph separator, or any Unicode
/// format character (general category Cf): zero-width and joiner
/// characters, bidirectional controls, the byte order mark, interlinear
/// annotations, and the invisible tag characters.
///
/// The list is the Cf category of Unicode 15.1.
pub(crate) fn is_unprintable(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host could send a text of any size, with newlines that forge log
    /// lines, escape sequences that drive a terminal, and a bidirectional
    /// override that reverses what a reader sees.
    #[test]
    fn bounded_replaces_control_characters_and_cuts_a_long_text() {
        let text: String = "refused\n\u{1b}[2J\u{1b}[31mred\u{202E}\r\n"
            .chars()
            .cycle()
            .take(10 * 1024)
            .collect();

        let message = bounded(&text);

        let suffix = " ... (9216 more characters)";
        assert!(message.ends_with(suffix), "{message}");
        assert_eq!(message.chars().count(), 1024 + suffix.len());
        assert!(
            message.starts_with("refused  [2J [31mred   refused "),
            "{message}"
        );
        assert!(!message.chars().any(is_unprintable), "{message}");
    }

    /// Tag characters are invisible, so they can hide text in a log. A soft
    /// hyphen and an Arabic letter mark are format characters too.
    #[test]
    fn bounded_replaces_tag_characters_and_other_format_characters() {
        assert_eq!(bounded("a\u{E0041}b\u{00AD}c\u{061C}d"), "a b c d");
    }

    #[test]
    fn bounded_keeps_a_text_of_exactly_the_cap() {
        let text = "x".repeat(MAX_ERROR_MESSAGE_CHARS);

        assert_eq!(bounded(&text), text);
    }
}
