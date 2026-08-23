//! Width-aware styled text helpers shared by the TUI renderers.

use super::{state::TuiState, theme::SemanticColor};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StyledTextPart {
    pub(crate) text: String,
    pub(crate) style: Style,
    pub(crate) atomic: bool,
}

pub(super) fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars().take(max_chars - 3).collect::<String>() + "..."
}

pub(super) fn inline_code_spans(
    state: &TuiState,
    text: &str,
    base_style: Style,
) -> Vec<Span<'static>> {
    inline_code_parts(state, text, base_style)
        .into_iter()
        .map(|part| Span::styled(part.text, part.style))
        .collect()
}

fn inline_code_parts(state: &TuiState, text: &str, base_style: Style) -> Vec<StyledTextPart> {
    let mut spans = Vec::new();
    let mut remainder = text;
    loop {
        let Some(start) = remainder.find('`') else {
            if !remainder.is_empty() {
                spans.push(StyledTextPart {
                    text: remainder.to_owned(),
                    style: base_style,
                    atomic: false,
                });
            }
            return spans;
        };
        let before = &remainder[..start];
        if !before.is_empty() {
            spans.push(StyledTextPart {
                text: before.to_owned(),
                style: base_style,
                atomic: false,
            });
        }
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('`') else {
            spans.push(StyledTextPart {
                text: remainder[start..].to_owned(),
                style: base_style,
                atomic: false,
            });
            return spans;
        };
        let code = &after_start[..end];
        spans.push(StyledTextPart {
            text: format!(" {code} "),
            style: inline_code_style(state, base_style),
            atomic: true,
        });
        remainder = &after_start[end + 1..];
    }
}

pub(crate) fn wrap_styled_parts(
    parts: Vec<StyledTextPart>,
    region_width: u16,
) -> Vec<Line<'static>> {
    wrap_styled_parts_with_policy(parts, region_width, false)
}

pub(crate) fn wrap_styled_parts_preserving_leading_whitespace(
    parts: Vec<StyledTextPart>,
    region_width: u16,
) -> Vec<Line<'static>> {
    wrap_styled_parts_with_policy(parts, region_width, true)
}

fn wrap_styled_parts_with_policy(
    parts: Vec<StyledTextPart>,
    region_width: u16,
    preserve_leading_whitespace: bool,
) -> Vec<Line<'static>> {
    let max_width = usize::from(region_width).max(1);
    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut current_width = 0;

    for part in parts {
        if part.atomic {
            push_atomic_part(
                part,
                max_width,
                &mut current,
                &mut current_width,
                &mut lines,
            );
        } else {
            push_wrappable_part(
                part,
                max_width,
                &mut current,
                &mut current_width,
                &mut lines,
                preserve_leading_whitespace,
            );
        }
    }

    lines.push(Line::from(current));
    lines
}

fn push_atomic_part(
    part: StyledTextPart,
    max_width: usize,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    lines: &mut Vec<Line<'static>>,
) {
    let width = UnicodeWidthStr::width(part.text.as_str());
    if *current_width > 0 && *current_width + width > max_width {
        lines.push(Line::from(std::mem::take(current)));
        *current_width = 0;
    }
    *current_width += width;
    current.push(Span::styled(part.text, part.style));
}

fn push_wrappable_part(
    part: StyledTextPart,
    max_width: usize,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    lines: &mut Vec<Line<'static>>,
    preserve_leading_whitespace: bool,
) {
    for token in wrap_tokens(&part.text) {
        push_wrappable_token(
            token,
            part.style,
            max_width,
            current,
            current_width,
            lines,
            preserve_leading_whitespace,
        );
    }
}

fn wrap_tokens(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut tokens = Vec::new();
    let mut token_start = 0;
    let mut previous_was_whitespace: Option<bool> = None;
    for (index, character) in text.char_indices() {
        let is_whitespace = character.is_whitespace();
        if let Some(previous) = previous_was_whitespace
            && previous != is_whitespace
        {
            tokens.push(&text[token_start..index]);
            token_start = index;
        }
        previous_was_whitespace = Some(is_whitespace);
    }
    tokens.push(&text[token_start..]);
    tokens
}

fn push_wrappable_token(
    token: &str,
    style: Style,
    max_width: usize,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    lines: &mut Vec<Line<'static>>,
    preserve_leading_whitespace: bool,
) {
    if token.chars().all(char::is_whitespace) {
        push_whitespace_token(
            token,
            style,
            max_width,
            current,
            current_width,
            lines,
            preserve_leading_whitespace,
        );
        return;
    }

    let token_width = UnicodeWidthStr::width(token);
    if token_width <= max_width {
        if *current_width > 0 && *current_width + token_width > max_width {
            lines.push(Line::from(std::mem::take(current)));
            *current_width = 0;
        }
        current.push(Span::styled(token.to_owned(), style));
        *current_width += token_width;
        return;
    }

    push_long_token_by_char(token, style, max_width, current, current_width, lines);
}

fn push_whitespace_token(
    token: &str,
    style: Style,
    max_width: usize,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    lines: &mut Vec<Line<'static>>,
    preserve_leading_whitespace: bool,
) {
    if *current_width == 0 && !preserve_leading_whitespace {
        return;
    }

    let token_width = UnicodeWidthStr::width(token);
    if token_width > max_width {
        push_long_token_by_char(token, style, max_width, current, current_width, lines);
        return;
    }

    if *current_width > 0 && *current_width + token_width > max_width {
        lines.push(Line::from(std::mem::take(current)));
        *current_width = 0;
        return;
    }

    current.push(Span::styled(token.to_owned(), style));
    *current_width += token_width;
}

fn push_long_token_by_char(
    token: &str,
    style: Style,
    max_width: usize,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    lines: &mut Vec<Line<'static>>,
) {
    let mut chunk = String::new();
    for character in token.chars() {
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if *current_width > 0 && *current_width + width > max_width {
            if !chunk.is_empty() {
                current.push(Span::styled(std::mem::take(&mut chunk), style));
            }
            lines.push(Line::from(std::mem::take(current)));
            *current_width = 0;
        }
        chunk.push(character);
        *current_width += width;
    }
    if !chunk.is_empty() {
        current.push(Span::styled(chunk, style));
    }
}

fn inline_code_style(state: &TuiState, base_style: Style) -> Style {
    let foreground = state.theme().color(SemanticColor::Focus);
    let mut style = base_style.add_modifier(Modifier::BOLD);
    if let Some(foreground) = foreground {
        style = style.fg(foreground);
    }
    style
}

pub(super) fn semantic_style(state: &TuiState, slot: SemanticColor) -> Style {
    state
        .theme()
        .color(slot)
        .map_or_else(Style::default, |color| Style::default().fg(color))
}
