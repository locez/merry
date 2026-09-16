//! Presentation-safe text helpers shared by the CLI surfaces.
//!
//! Model, provider, and process text reaches the terminal as written, so
//! control characters are sanitized in one place instead of once per renderer.
//! These helpers only prepare text for display; they never decide what is
//! shown or what a tool is allowed to do.

/// Drops control characters so text stays on a single line.
pub(crate) fn without_control_chars(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

/// Replaces control characters with spaces so separate words stay separate.
///
/// A newline or tab inside a command or path otherwise joins the words on both
/// sides of it, which reads as a different value than the one that ran.
pub(crate) fn with_control_chars_as_spaces(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// Drops control characters except line breaks, which keep their lines.
pub(crate) fn without_control_chars_keeping_newlines(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_policy_sanitizes_control_characters_its_own_way() {
        let text = "a\tb\nc\rd\u{1b}";

        assert_eq!(without_control_chars(text), "abcd");
        assert_eq!(with_control_chars_as_spaces(text), "a b c d ");
        assert_eq!(without_control_chars_keeping_newlines(text), "ab\ncd");
    }

    #[test]
    fn printable_text_is_returned_unchanged() {
        let text = "正常 text with spaces  and emoji 🚀";

        assert_eq!(without_control_chars(text), text);
        assert_eq!(with_control_chars_as_spaces(text), text);
        assert_eq!(without_control_chars_keeping_newlines(text), text);
    }
}
