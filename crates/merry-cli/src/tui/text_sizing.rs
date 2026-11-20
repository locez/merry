//! Progressive Kitty `OSC 66` text-sizing support (Requirement #3, Layer B).
//!
//! Disabled by default (`text_sizing = "off"`). When set to `"auto"`, enables
//! only on terminals advertising Kitty/Ghostty/WezTerm support. When active,
//! callers can reserve vertical space in ratatui layout and emit `OSC 66`
//! scaled text spans inside the same `DECSET 2026` synchronized frame.

use crate::config::TextSizingMode;
use std::io::{self, Write};

/// A scaled text run to emit directly after ratatui finishes its frame diff
/// (inside the same synchronized update block so there is no flicker).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct Osc66Span {
    /// 0-based terminal column.
    pub(crate) col: u16,
    /// 0-based terminal row.
    pub(crate) row: u16,
    /// Integer scale factor (2 = double size).
    pub(crate) scale: u8,
    /// Plain text to render at `scale`.
    pub(crate) text: String,
}

/// Resolves whether Kitty `OSC 66` text sizing should be active for the current
/// terminal environment and user preference.
pub(crate) fn is_text_sizing_active(mode: TextSizingMode) -> bool {
    match mode {
        TextSizingMode::Off => false,
        TextSizingMode::On => true,
        TextSizingMode::Auto => detect_osc66_support_from_env(
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
            std::env::var("KITTY_WINDOW_ID").ok().as_deref(),
        ),
    }
}

pub(crate) fn detect_osc66_support_from_env(
    term_program: Option<&str>,
    term: Option<&str>,
    kitty_window_id: Option<&str>,
) -> bool {
    if kitty_window_id.is_some_and(|value| !value.is_empty()) {
        return true;
    }
    if let Some(program) = term_program {
        let lower = program.to_ascii_lowercase();
        if lower.contains("kitty") || lower.contains("ghostty") {
            return true;
        }
    }
    if let Some(term) = term {
        let lower = term.to_ascii_lowercase();
        if lower.contains("kitty") || lower.contains("ghostty") {
            return true;
        }
    }
    false
}

/// Formats a single `OSC 66` escape sequence at 1-based cursor `(row+1, col+1)`.
#[allow(dead_code)]
pub(crate) fn format_osc66_span(span: &Osc66Span) -> String {
    let row = span.row.saturating_add(1);
    let col = span.col.saturating_add(1);
    let scale = span.scale.clamp(2, 4);
    let safe_text: String = span.text.chars().filter(|ch| !ch.is_control()).collect();
    format!("\x1b[{row};{col}H\x1b]66;s={scale};{safe_text}\x07")
}

/// Emits queued `OSC 66` spans directly to the terminal writer.
#[allow(dead_code)]
pub(crate) fn emit_osc66_spans(writer: &mut impl Write, spans: &[Osc66Span]) -> io::Result<()> {
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        writer.write_all(format_osc66_span(span).as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_off_and_on_respect_explicit_user_choice() {
        assert!(!is_text_sizing_active(TextSizingMode::Off));
        assert!(is_text_sizing_active(TextSizingMode::On));
    }

    #[test]
    fn auto_detection_recognizes_kitty_and_ghostty_without_false_positives() {
        assert!(detect_osc66_support_from_env(Some("ghostty"), None, None));
        assert!(detect_osc66_support_from_env(
            None,
            Some("xterm-kitty"),
            None
        ));
        assert!(detect_osc66_support_from_env(None, None, Some("1")));
        assert!(!detect_osc66_support_from_env(
            Some("Apple_Terminal"),
            Some("xterm-256color"),
            None
        ));
    }

    #[test]
    fn osc66_formatter_strips_control_chars_and_uses_one_based_coordinates() {
        let span = Osc66Span {
            col: 2,
            row: 4,
            scale: 2,
            text: "Title\x1b[31m".to_owned(),
        };
        let mut out = Vec::new();
        emit_osc66_spans(&mut out, &[span]).expect("write should succeed");
        let text = String::from_utf8(out).expect("utf8");
        assert_eq!(text, "\x1b[5;3H\x1b]66;s=2;Title[31m\x07");
    }
}
