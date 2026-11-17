use crate::tui::{
    state::TuiState,
    text_wrap::semantic_style,
    theme::{SemanticColor, dim_color},
};
use ratatui::{
    style::Modifier,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

const COPY_ICON: &str = "\u{29C9}";
const COPY_LABEL: &str = "\u{29C9} Copy";

pub(crate) const MAX_CLIPBOARD_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyTextError {
    TooLarge,
}

pub(crate) struct CopyTextBuilder {
    text: String,
}

impl CopyTextBuilder {
    pub(crate) fn new() -> Self {
        Self {
            text: String::new(),
        }
    }

    pub(crate) fn push_str(&mut self, chunk: &str) -> Result<(), CopyTextError> {
        let next_len = self
            .text
            .len()
            .checked_add(chunk.len())
            .ok_or(CopyTextError::TooLarge)?;
        if next_len > MAX_CLIPBOARD_BYTES {
            return Err(CopyTextError::TooLarge);
        }
        self.text.push_str(chunk);
        Ok(())
    }

    pub(crate) fn finish(self) -> String {
        self.text
    }
}

pub(crate) fn validate_copy_text(text: String) -> Result<String, CopyTextError> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        Err(CopyTextError::TooLarge)
    } else {
        Ok(text)
    }
}

/// Code owns its parsed text; replies refer to the existing timeline instead of duplicating it.
pub(crate) enum CopyContent {
    Text(String),
    AssistantMessage(usize),
}

/// Associates an actual copy control with its undecorated source and logical row.
pub(crate) struct CopyTarget {
    pub(crate) line_index: usize,
    pub(crate) column: u16,
    pub(crate) width: u16,
    pub(crate) content: CopyContent,
}

impl CopyTarget {
    pub(crate) fn new(line_index: usize, width: u16, content: CopyContent) -> Self {
        Self {
            line_index,
            column: 0,
            width,
            content,
        }
    }
}

/// Renders the copy chip, or omits it when `⧉ Copy` cannot fit.
pub(crate) fn copy_header(state: &TuiState, width: u16) -> Option<(Line<'static>, u16)> {
    let columns = UnicodeWidthStr::width(COPY_LABEL);
    if usize::from(width) < columns {
        return None;
    }
    let (icon, rest) = COPY_LABEL.split_at(COPY_ICON.len());
    let band = state.theme().color(SemanticColor::Focus).map(dim_color);
    let mut icon_style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let mut word_style =
        semantic_style(state, SemanticColor::Assistant).add_modifier(Modifier::BOLD);
    if let Some(color) = band {
        icon_style = icon_style.bg(color);
        word_style = word_style.bg(color);
    }
    let line = Line::from(vec![
        Span::styled(icon.to_owned(), icon_style),
        Span::styled(rest.to_owned(), word_style),
    ]);
    Some((line, u16::try_from(columns).unwrap_or(u16::MAX)))
}

/// Status text after a copy attempt. Either the system clipboard or the terminal write is enough.
pub(crate) fn copy_result_feedback(
    system_ok: bool,
    terminal_ok: bool,
    system_error: &str,
) -> (String, bool) {
    if system_ok || terminal_ok {
        ("Copied".to_owned(), false)
    } else {
        (format!("Copy failed: {system_error}"), true)
    }
}

#[cfg(test)]
mod tests {
    use super::{CopyTextBuilder, MAX_CLIPBOARD_BYTES};

    #[test]
    fn copy_builder_rejects_the_first_chunk_over_the_limit() {
        let mut builder = CopyTextBuilder::new();
        builder
            .push_str(&"x".repeat(MAX_CLIPBOARD_BYTES - 1))
            .expect("payload below the limit should be accepted");
        builder
            .push_str("x")
            .expect("payload at the limit should be accepted");
        assert_eq!(builder.push_str("x"), Err(super::CopyTextError::TooLarge));
        assert_eq!(builder.finish().len(), MAX_CLIPBOARD_BYTES);
    }

    #[test]
    fn copy_feedback_prefers_a_short_success_message() {
        assert_eq!(
            super::copy_result_feedback(true, false, "unused"),
            ("Copied".to_owned(), false)
        );
        assert_eq!(
            super::copy_result_feedback(false, true, "clipboard unavailable"),
            ("Copied".to_owned(), false)
        );
        assert_eq!(
            super::copy_result_feedback(false, false, "clipboard unavailable"),
            ("Copy failed: clipboard unavailable".to_owned(), true)
        );
    }

    #[test]
    fn copy_labels_use_display_columns() {
        assert_eq!(unicode_width::UnicodeWidthStr::width("\u{29C9} Copy"), 6);
        assert_eq!(unicode_width::UnicodeWidthStr::width("\u{276F} "), 2);
    }
}
