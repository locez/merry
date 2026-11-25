use crate::{
    config::CopyButtonPosition,
    tui::{state::TuiState, text_wrap::semantic_style, theme::SemanticColor},
};
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

pub(crate) fn validate_copy_text(text: &str) -> Result<String, CopyTextError> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        Err(CopyTextError::TooLarge)
    } else {
        Ok(text.to_owned())
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

/// Rendered copy-control line together with its clickable column range.
pub(crate) struct CopyBar {
    pub(crate) line: Line<'static>,
    pub(crate) column: u16,
    pub(crate) width: u16,
}

/// Which edge of a fenced code block carries its clickable copy button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeBlockCopyEdge {
    Top,
    Bottom,
}

/// Clickable copy hit range relative to a code block's top or bottom chrome row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodeBlockCopyHit {
    pub(crate) edge: CodeBlockCopyEdge,
    pub(crate) column: u16,
    pub(crate) width: u16,
}

/// Optional top and bottom chrome rows for a fenced code block plus its copy hit target.
pub(crate) struct CodeBlockChrome {
    pub(crate) top_bar: Option<Line<'static>>,
    pub(crate) bottom_bar: Option<Line<'static>>,
    pub(crate) copy_hit: Option<CodeBlockCopyHit>,
}

impl CodeBlockChrome {
    const fn empty() -> Self {
        Self {
            top_bar: None,
            bottom_bar: None,
            copy_hit: None,
        }
    }
}

/// Renders a quiet copy chip for an assistant reply aligned to the configured
/// side (`TopLeft`/`BottomLeft` on the left, `TopRight`/`BottomRight` on the
/// right), or omits it when `⧉ Copy` cannot fit.
pub(crate) fn copy_bar(
    state: &TuiState,
    width: u16,
    position: CopyButtonPosition,
) -> Option<CopyBar> {
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
    let col = if position.is_left() {
        0
    } else {
        total.saturating_sub(columns)
    };
    let mut spans = Vec::with_capacity(3);
    if col > 0 {
        spans.push(Span::raw(" ".repeat(col)));
    }
    spans.push(Span::styled(icon.to_owned(), icon_style));
    spans.push(Span::styled(rest.to_owned(), word_style));
    Some(CopyBar {
        line: Line::from(spans),
        column: u16::try_from(col).unwrap_or(0),
        width: u16::try_from(columns).unwrap_or(u16::MAX),
    })
}

const CODE_BLOCK_RAIL: &str = "▎ ";

struct CodeBlockBarTheme {
    has_code_bg: bool,
    code_bg_style: Style,
    rail_style: Style,
    icon_style: Style,
    word_style: Style,
}

impl CodeBlockBarTheme {
    fn from_state(state: &TuiState) -> Self {
        let code_bg = state.theme().bg_color(SemanticColor::CodeBackground);
        let code_bg_style = code_bg.map_or_else(Style::default, |bg| Style::default().bg(bg));
        let rail_style = semantic_style(state, SemanticColor::Status)
            .add_modifier(Modifier::BOLD)
            .patch(code_bg_style);
        let chip_bg = state.theme().bg_color(SemanticColor::ChipBackground);
        let mut icon_style =
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
        let mut word_style =
            semantic_style(state, SemanticColor::Assistant).add_modifier(Modifier::BOLD);
        if let Some(color) = chip_bg {
            icon_style = icon_style.bg(color);
            word_style = word_style.bg(color);
        }
        Self {
            has_code_bg: code_bg.is_some(),
            code_bg_style,
            rail_style,
            icon_style,
            word_style,
        }
    }

    fn push_copy_chip(&self, spans: &mut Vec<Span<'static>>) {
        let (icon, rest) = COPY_LABEL.split_at(COPY_ICON.len());
        spans.push(Span::styled(icon.to_owned(), self.icon_style));
        spans.push(Span::styled(rest.to_owned(), self.word_style));
    }

    fn push_trailing_bg(&self, spans: &mut Vec<Span<'static>>, total: usize, used: usize) {
        if self.has_code_bg && total > used {
            spans.push(Span::styled(" ".repeat(total - used), self.code_bg_style));
        }
    }
}

/// Renders the top and optional bottom chrome bars for a fenced code block
/// according to the active [`CopyButtonPosition`].
pub(crate) fn code_block_chrome(
    state: &TuiState,
    lang: Option<&str>,
    width: u16,
) -> CodeBlockChrome {
    let position = state.copy_button_position();
    let copy_columns = UnicodeWidthStr::width(COPY_LABEL);
    let copy_width = u16::try_from(copy_columns).unwrap_or(u16::MAX);
    let total = usize::from(width);
    let theme = CodeBlockBarTheme::from_state(state);
    let rail_width = UnicodeWidthStr::width(CODE_BLOCK_RAIL);

    let trimmed_lang = lang.map(str::trim).filter(|value| !value.is_empty());
    let label = trimmed_lang.unwrap_or("code");
    let label_style = if trimmed_lang.is_some() {
        semantic_style(state, SemanticColor::ToolKeyword)
            .add_modifier(Modifier::BOLD)
            .patch(theme.code_bg_style)
    } else {
        semantic_style(state, SemanticColor::Muted).patch(theme.code_bg_style)
    };
    let label_width = UnicodeWidthStr::width(label);
    let left_width = rail_width + label_width;

    match position {
        CopyButtonPosition::TopRight => {
            if total < copy_columns {
                return CodeBlockChrome::empty();
            }
            let right_pad = usize::from(total >= left_width + copy_columns + 3);
            if total < left_width + copy_columns + 1 {
                return fallback_top_copy_chrome(state, width, position);
            }
            let gap = total.saturating_sub(left_width + copy_columns + right_pad);
            let mut spans = vec![
                Span::styled(CODE_BLOCK_RAIL.to_owned(), theme.rail_style),
                Span::styled(label.to_owned(), label_style),
                Span::styled(" ".repeat(gap), theme.code_bg_style),
            ];
            theme.push_copy_chip(&mut spans);
            if right_pad > 0 {
                spans.push(Span::styled(" ".repeat(right_pad), theme.code_bg_style));
            }
            CodeBlockChrome {
                top_bar: Some(Line::from(spans)),
                bottom_bar: None,
                copy_hit: Some(CodeBlockCopyHit {
                    edge: CodeBlockCopyEdge::Top,
                    column: u16::try_from(left_width + gap).unwrap_or(u16::MAX),
                    width: copy_width,
                }),
            }
        }
        CopyButtonPosition::TopLeft => {
            if total < copy_columns {
                return CodeBlockChrome::empty();
            }
            if total < rail_width + copy_columns {
                return fallback_top_copy_chrome(state, width, position);
            }
            let mut spans = vec![Span::styled(CODE_BLOCK_RAIL.to_owned(), theme.rail_style)];
            theme.push_copy_chip(&mut spans);
            let mut used = rail_width + copy_columns;
            if total >= used + 2 + label_width {
                spans.push(Span::styled("  ".to_owned(), theme.code_bg_style));
                spans.push(Span::styled(label.to_owned(), label_style));
                used += 2 + label_width;
            }
            theme.push_trailing_bg(&mut spans, total, used);
            CodeBlockChrome {
                top_bar: Some(Line::from(spans)),
                bottom_bar: None,
                copy_hit: Some(CodeBlockCopyHit {
                    edge: CodeBlockCopyEdge::Top,
                    column: u16::try_from(rail_width).unwrap_or(u16::MAX),
                    width: copy_width,
                }),
            }
        }
        CopyButtonPosition::BottomLeft | CopyButtonPosition::BottomRight => {
            let top_bar = (total >= left_width).then(|| {
                let mut spans = vec![
                    Span::styled(CODE_BLOCK_RAIL.to_owned(), theme.rail_style),
                    Span::styled(label.to_owned(), label_style),
                ];
                theme.push_trailing_bg(&mut spans, total, left_width);
                Line::from(spans)
            });
            let (bottom_bar, copy_hit) = match code_block_bottom_bar(
                state,
                &theme,
                width,
                position,
                rail_width,
                copy_columns,
                copy_width,
            ) {
                Some(bar) => (
                    Some(bar.line),
                    Some(CodeBlockCopyHit {
                        edge: CodeBlockCopyEdge::Bottom,
                        column: bar.column,
                        width: bar.width,
                    }),
                ),
                None => (None, None),
            };
            CodeBlockChrome {
                top_bar,
                bottom_bar,
                copy_hit,
            }
        }
    }
}

fn fallback_top_copy_chrome(
    state: &TuiState,
    width: u16,
    position: CopyButtonPosition,
) -> CodeBlockChrome {
    let Some(bar) = copy_bar(state, width, position) else {
        return CodeBlockChrome::empty();
    };
    CodeBlockChrome {
        top_bar: Some(bar.line),
        bottom_bar: None,
        copy_hit: Some(CodeBlockCopyHit {
            edge: CodeBlockCopyEdge::Top,
            column: bar.column,
            width: bar.width,
        }),
    }
}

fn code_block_bottom_bar(
    state: &TuiState,
    theme: &CodeBlockBarTheme,
    width: u16,
    position: CopyButtonPosition,
    rail_width: usize,
    copy_columns: usize,
    copy_width: u16,
) -> Option<CopyBar> {
    let total = usize::from(width);
    if total < copy_columns {
        return None;
    }
    if total < rail_width + copy_columns {
        return copy_bar(state, width, position);
    }
    let mut spans = vec![Span::styled(CODE_BLOCK_RAIL.to_owned(), theme.rail_style)];
    let column = if position.is_left() {
        theme.push_copy_chip(&mut spans);
        theme.push_trailing_bg(&mut spans, total, rail_width + copy_columns);
        rail_width
    } else {
        let right_pad = usize::from(total >= rail_width + copy_columns + 2);
        let gap = total.saturating_sub(rail_width + copy_columns + right_pad);
        if gap > 0 {
            spans.push(Span::styled(" ".repeat(gap), theme.code_bg_style));
        }
        theme.push_copy_chip(&mut spans);
        if right_pad > 0 {
            spans.push(Span::styled(" ".repeat(right_pad), theme.code_bg_style));
        }
        rail_width + gap
    };
    Some(CopyBar {
        line: Line::from(spans),
        column: u16::try_from(column).unwrap_or(u16::MAX),
        width: copy_width,
    })
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
