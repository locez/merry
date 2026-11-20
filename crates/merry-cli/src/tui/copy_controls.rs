use crate::tui::{state::TuiState, text_wrap::semantic_style, theme::SemanticColor};
use ratatui::{
    style::{Modifier, Style},
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CopyContent {
    Text(String),
    AssistantMessage(usize),
}

/// Associates an actual copy control with its undecorated source and logical row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyTarget {
    pub(crate) line_index: usize,
    pub(crate) column: u16,
    pub(crate) width: u16,
    pub(crate) content: CopyContent,
}

impl CopyTarget {
    pub(crate) fn new(line_index: usize, column: u16, width: u16, content: CopyContent) -> Self {
        Self {
            line_index,
            column,
            width,
            content,
        }
    }
}

/// Renders a quiet right-aligned copy chip for an assistant reply, or omits it
/// when `⧉ Copy` cannot fit. Returns `(line, column, width)`.
pub(crate) fn copy_header(state: &TuiState, width: u16) -> Option<(Line<'static>, u16, u16)> {
    let columns = UnicodeWidthStr::width(COPY_LABEL);
    let total = usize::from(width);
    if total < columns {
        return None;
    }
    let (icon, rest) = COPY_LABEL.split_at(COPY_ICON.len());
    let band = state.theme().bg_color(SemanticColor::ChipBackground);
    let mut icon_style = semantic_style(state, SemanticColor::Focus);
    let mut word_style = semantic_style(state, SemanticColor::Muted);
    if let Some(color) = band {
        icon_style = icon_style.bg(color);
        word_style = word_style.bg(color);
    }
    let col = total.saturating_sub(columns);
    let mut spans = Vec::with_capacity(3);
    if col > 0 {
        spans.push(Span::raw(" ".repeat(col)));
    }
    spans.push(Span::styled(icon.to_owned(), icon_style));
    spans.push(Span::styled(rest.to_owned(), word_style));
    Some((
        Line::from(spans),
        u16::try_from(col).unwrap_or(0),
        u16::try_from(columns).unwrap_or(u16::MAX),
    ))
}

/// Renders a code-block card header with the language badge on the left and
/// a right-aligned clickable `⧉ Copy` pill on `CodeBackground`.
///
/// Returns `(header_line, copy_column, copy_width)`.
pub(crate) fn code_block_header(
    state: &TuiState,
    lang: Option<&str>,
    width: u16,
) -> Option<(Line<'static>, u16, u16)> {
    let copy_columns = UnicodeWidthStr::width(COPY_LABEL);
    let total = usize::from(width);
    if total < copy_columns {
        return None;
    }
    let code_bg = state.theme().bg_color(SemanticColor::CodeBackground);
    let code_bg_style = code_bg.map_or_else(Style::default, |bg| Style::default().bg(bg));
    let rail_style = semantic_style(state, SemanticColor::Status)
        .add_modifier(Modifier::BOLD)
        .patch(code_bg_style);
    let (icon, rest) = COPY_LABEL.split_at(COPY_ICON.len());
    let chip_bg = state.theme().bg_color(SemanticColor::ChipBackground);
    let mut icon_style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let mut word_style =
        semantic_style(state, SemanticColor::Assistant).add_modifier(Modifier::BOLD);
    if let Some(color) = chip_bg {
        icon_style = icon_style.bg(color);
        word_style = word_style.bg(color);
    }

    let label = lang
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("code");
    let label_style = if lang.is_some_and(|value| !value.trim().is_empty()) {
        semantic_style(state, SemanticColor::ToolKeyword)
            .add_modifier(Modifier::BOLD)
            .patch(code_bg_style)
    } else {
        semantic_style(state, SemanticColor::Muted).patch(code_bg_style)
    };
    let rail = "▎ ";
    let left_width = UnicodeWidthStr::width(rail) + UnicodeWidthStr::width(label);
    let right_pad = usize::from(total >= left_width + copy_columns + 3);
    if total < left_width + copy_columns + 1 {
        let (line, col, w) = copy_header(state, width)?;
        return Some((line, col, w));
    }
    let gap = total.saturating_sub(left_width + copy_columns + right_pad);
    let copy_col = u16::try_from(left_width + gap).unwrap_or(u16::MAX);
    let copy_w = u16::try_from(copy_columns).unwrap_or(u16::MAX);
    let mut spans = vec![
        Span::styled(rail.to_owned(), rail_style),
        Span::styled(label.to_owned(), label_style),
        Span::styled(" ".repeat(gap), code_bg_style),
        Span::styled(icon.to_owned(), icon_style),
        Span::styled(rest.to_owned(), word_style),
    ];
    if right_pad > 0 {
        spans.push(Span::styled(" ".repeat(right_pad), code_bg_style));
    }
    Some((Line::from(spans), copy_col, copy_w))
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
