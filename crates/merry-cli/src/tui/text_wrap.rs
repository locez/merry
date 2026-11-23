//! Width-aware styled text helpers shared by the TUI renderers.

use super::{links::LineLink, state::TuiState, theme::SemanticColor};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use std::sync::Arc;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StyledTextPart {
    pub(crate) text: String,
    pub(crate) style: Style,
    pub(crate) atomic: bool,
    pub(crate) link: Option<Arc<str>>,
}

impl StyledTextPart {
    pub(crate) fn new(text: String, style: Style, atomic: bool) -> Self {
        Self {
            text,
            style,
            atomic,
            link: None,
        }
    }

    pub(crate) fn with_link(
        text: String,
        style: Style,
        atomic: bool,
        link: Option<Arc<str>>,
    ) -> Self {
        Self {
            text,
            style,
            atomic,
            link,
        }
    }
}

/// A wrapped display line together with exact column-aligned hyperlink ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WrappedLine {
    pub(crate) line: Line<'static>,
    pub(crate) links: Vec<LineLink>,
}

impl WrappedLine {
    pub(crate) fn plain(line: Line<'static>) -> Self {
        Self {
            line,
            links: Vec::new(),
        }
    }

    pub(crate) fn with_links(line: Line<'static>, links: Vec<LineLink>) -> Self {
        Self { line, links }
    }

    pub(crate) fn shift_links(&mut self, offset: usize) {
        let delta = u16::try_from(offset).unwrap_or(u16::MAX);
        for link in &mut self.links {
            link.column = link.column.saturating_add(delta);
        }
    }
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
                spans.push(StyledTextPart::new(remainder.to_owned(), base_style, false));
            }
            return spans;
        };
        let before = &remainder[..start];
        if !before.is_empty() {
            spans.push(StyledTextPart::new(before.to_owned(), base_style, false));
        }
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('`') else {
            spans.push(StyledTextPart::new(
                remainder[start..].to_owned(),
                base_style,
                false,
            ));
            return spans;
        };
        let code = &after_start[..end];
        spans.push(StyledTextPart::new(
            inline_code_text(state, code),
            inline_code_style(state, base_style),
            true,
        ));
        remainder = &after_start[end + 1..];
    }
}

pub(crate) fn wrap_styled_parts(
    parts: Vec<StyledTextPart>,
    region_width: u16,
) -> Vec<Line<'static>> {
    wrap_styled_parts_with_links(parts, region_width)
        .into_iter()
        .map(|wrapped| wrapped.line)
        .collect()
}

pub(crate) fn wrap_styled_parts_with_links(
    parts: Vec<StyledTextPart>,
    region_width: u16,
) -> Vec<WrappedLine> {
    wrap_styled_parts_with_policy(parts, region_width, false)
}

pub(crate) fn wrap_styled_parts_preserving_leading_whitespace(
    parts: Vec<StyledTextPart>,
    region_width: u16,
) -> Vec<Line<'static>> {
    wrap_styled_parts_with_policy(parts, region_width, true)
        .into_iter()
        .map(|wrapped| wrapped.line)
        .collect()
}

struct WrapAccumulator {
    lines: Vec<WrappedLine>,
    current: Vec<Span<'static>>,
    current_links: Vec<LineLink>,
    current_width: usize,
    max_width: usize,
}

impl WrapAccumulator {
    fn new(max_width: usize) -> Self {
        Self {
            lines: Vec::new(),
            current: Vec::new(),
            current_links: Vec::new(),
            current_width: 0,
            max_width,
        }
    }

    fn flush_line(&mut self) {
        self.lines.push(WrappedLine::with_links(
            Line::from(std::mem::take(&mut self.current)),
            std::mem::take(&mut self.current_links),
        ));
        self.current_width = 0;
    }

    fn push_span(&mut self, text: String, style: Style, width: usize, link: Option<&Arc<str>>) {
        let start_col = self.current_width;
        self.current.push(Span::styled(text, style));
        self.current_width += width;
        if let Some(url) = link
            && width > 0
        {
            let col = u16::try_from(start_col).unwrap_or(u16::MAX);
            let span_width = u16::try_from(width).unwrap_or(u16::MAX);
            if let Some(last) = self.current_links.last_mut()
                && last.url == url.as_ref()
                && last.column.saturating_add(last.width) == col
            {
                last.width = last.width.saturating_add(span_width);
            } else {
                self.current_links
                    .push(LineLink::new(col, span_width, url.as_ref().to_owned()));
            }
        }
    }

    fn finish(mut self) -> Vec<WrappedLine> {
        self.flush_line();
        self.lines
    }
}

fn wrap_styled_parts_with_policy(
    parts: Vec<StyledTextPart>,
    region_width: u16,
    preserve_leading_whitespace: bool,
) -> Vec<WrappedLine> {
    let max_width = usize::from(region_width).max(1);
    let mut acc = WrapAccumulator::new(max_width);

    for part in parts {
        if part.atomic {
            push_atomic_part(part, &mut acc);
        } else {
            push_wrappable_part(part, &mut acc, preserve_leading_whitespace);
        }
    }

    acc.finish()
}

fn push_atomic_part(part: StyledTextPart, acc: &mut WrapAccumulator) {
    let width = UnicodeWidthStr::width(part.text.as_str());
    if width <= acc.max_width {
        if acc.current_width > 0 && acc.current_width + width > acc.max_width {
            acc.flush_line();
        }
        acc.push_span(part.text, part.style, width, part.link.as_ref());
        return;
    }
    if acc.current_width > 0 {
        acc.flush_line();
    }
    push_long_token_by_char(&part.text, part.style, part.link.as_ref(), acc);
}

fn push_wrappable_part(
    part: StyledTextPart,
    acc: &mut WrapAccumulator,
    preserve_leading_whitespace: bool,
) {
    for token in wrap_tokens(&part.text) {
        push_wrappable_token(
            token,
            part.style,
            part.link.as_ref(),
            acc,
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
    link: Option<&Arc<str>>,
    acc: &mut WrapAccumulator,
    preserve_leading_whitespace: bool,
) {
    if token.chars().all(char::is_whitespace) {
        push_whitespace_token(token, style, link, acc, preserve_leading_whitespace);
        return;
    }

    let token_width = UnicodeWidthStr::width(token);
    if token_width <= acc.max_width {
        if acc.current_width > 0 && acc.current_width + token_width > acc.max_width {
            acc.flush_line();
        }
        acc.push_span(token.to_owned(), style, token_width, link);
        return;
    }

    push_long_token_by_char(token, style, link, acc);
}

fn push_whitespace_token(
    token: &str,
    style: Style,
    link: Option<&Arc<str>>,
    acc: &mut WrapAccumulator,
    preserve_leading_whitespace: bool,
) {
    if acc.current_width == 0 && !preserve_leading_whitespace {
        return;
    }

    let token_width = UnicodeWidthStr::width(token);
    if token_width > acc.max_width {
        push_long_token_by_char(token, style, link, acc);
        return;
    }

    if acc.current_width > 0 && acc.current_width + token_width > acc.max_width {
        acc.flush_line();
        return;
    }

    acc.push_span(token.to_owned(), style, token_width, link);
}

fn push_long_token_by_char(
    token: &str,
    style: Style,
    link: Option<&Arc<str>>,
    acc: &mut WrapAccumulator,
) {
    let mut chunk = String::new();
    let mut chunk_width = 0usize;
    for character in token.chars() {
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if acc.current_width + chunk_width > 0
            && acc.current_width + chunk_width + width > acc.max_width
        {
            if !chunk.is_empty() {
                acc.push_span(std::mem::take(&mut chunk), style, chunk_width, link);
                chunk_width = 0;
            }
            acc.flush_line();
        }
        chunk.push(character);
        chunk_width += width;
    }
    if !chunk.is_empty() {
        acc.push_span(chunk, style, chunk_width, link);
    }
}

pub(super) fn inline_code_text(state: &TuiState, code: &str) -> String {
    if state
        .theme()
        .bg_color(SemanticColor::InlineCodeBackground)
        .is_some()
    {
        format!(" {code} ")
    } else {
        code.to_owned()
    }
}

pub(super) fn inline_code_style(state: &TuiState, base_style: Style) -> Style {
    let foreground = state.theme().color(SemanticColor::Focus);
    let mut style = base_style.add_modifier(Modifier::BOLD);
    if let Some(foreground) = foreground {
        style = style.fg(foreground);
    }
    if let Some(background) = state.theme().bg_color(SemanticColor::InlineCodeBackground) {
        style = style.bg(background);
    }
    style
}

pub(super) fn semantic_style(state: &TuiState, slot: SemanticColor) -> Style {
    state
        .theme()
        .color(slot)
        .map_or_else(Style::default, |color| Style::default().fg(color))
}
