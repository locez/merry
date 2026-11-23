//! Display-only MCP activity row styling; mirrors `command_style` for shell rows.

use crate::tui::{
    state::TuiState,
    text_wrap::{inline_code_spans, semantic_style},
    theme::SemanticColor,
};
use ratatui::{
    style::Modifier,
    text::{Line, Span},
};

/// Styles an `MCP` activity title with the same visual hierarchy as `Ran` rows:
/// bold `ToolKeyword` prefix, `server/tool` accent (`Command + BOLD` on the
/// tool name), muted argument keys, and preserved trailing `(summary)` /
/// elapsed spans so collapsed rows truncate cleanly in the middle.
pub(super) fn mcp_title_line(state: &TuiState, detail: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "MCP".to_owned(),
        semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD),
    )];
    if detail.is_empty() {
        return Line::from(spans);
    }
    spans.push(Span::styled(
        " ".to_owned(),
        semantic_style(state, SemanticColor::Muted),
    ));

    let (body, tail_spans) = split_mcp_tail_spans(state, detail);
    if !body.is_empty() {
        append_mcp_body_spans(state, body, &mut spans);
    }
    spans.extend(tail_spans);
    Line::from(spans)
}

fn split_mcp_tail_spans<'a>(state: &TuiState, detail: &'a str) -> (&'a str, Vec<Span<'static>>) {
    let mut rest = detail;
    let mut tail_rev = Vec::new();

    if let Some((head, elapsed)) = split_elapsed_tail(rest) {
        tail_rev.push(Span::styled(
            elapsed.to_owned(),
            semantic_style(state, SemanticColor::Muted),
        ));
        rest = head;
    }

    if let Some(head) = rest.strip_suffix(" -> failed") {
        tail_rev.push(Span::styled(
            " -> failed".to_owned(),
            semantic_style(state, SemanticColor::Error).add_modifier(Modifier::BOLD),
        ));
        rest = head;
    } else if let Some(head) = rest.strip_suffix(" -> cancelled") {
        tail_rev.push(Span::styled(
            " -> cancelled".to_owned(),
            semantic_style(state, SemanticColor::Warning).add_modifier(Modifier::BOLD),
        ));
        rest = head;
    }

    if rest.ends_with(')')
        && let Some(index) = rest.rfind(" (")
        && !rest[index..].contains('"')
        && !rest[index..].contains('=')
    {
        tail_rev.push(Span::styled(
            rest[index..].to_owned(),
            semantic_style(state, SemanticColor::Muted),
        ));
        rest = &rest[..index];
    }

    tail_rev.reverse();
    (rest, tail_rev)
}

fn split_elapsed_tail(text: &str) -> Option<(&str, &str)> {
    let index = text.rfind("  ")?;
    let suffix = &text[index..];
    let body = suffix.strip_prefix("  ")?.strip_suffix('s')?;
    let (secs, tenths) = body.split_once('.')?;
    if !secs.is_empty()
        && secs.bytes().all(|byte| byte.is_ascii_digit())
        && tenths.len() == 1
        && tenths.bytes().all(|byte| byte.is_ascii_digit())
    {
        Some((&text[..index], suffix))
    } else {
        None
    }
}

fn append_mcp_body_spans(state: &TuiState, body: &str, spans: &mut Vec<Span<'static>>) {
    let (first, rest) = body.split_once(' ').unwrap_or((body, ""));
    if let Some((server, tool)) = first.split_once('/')
        && !server.is_empty()
        && !tool.is_empty()
    {
        spans.push(Span::styled(
            server.to_owned(),
            semantic_style(state, SemanticColor::TextSecondary),
        ));
        spans.push(Span::styled(
            "/".to_owned(),
            semantic_style(state, SemanticColor::Muted),
        ));
        spans.push(Span::styled(
            tool.to_owned(),
            semantic_style(state, SemanticColor::Command).add_modifier(Modifier::BOLD),
        ));
        if !rest.is_empty() {
            spans.push(Span::styled(
                " ".to_owned(),
                semantic_style(state, SemanticColor::Muted),
            ));
            append_mcp_arg_spans(state, rest, spans);
        }
        return;
    }

    if matches!(
        first,
        "searched" | "searching" | "refreshed" | "refreshing" | "listed" | "listing"
    ) {
        spans.push(Span::styled(
            first.to_owned(),
            semantic_style(state, SemanticColor::TextSecondary),
        ));
        if !rest.is_empty() {
            spans.push(Span::styled(
                " ".to_owned(),
                semantic_style(state, SemanticColor::Muted),
            ));
            if let Some((target, query)) = rest.split_once(" · ") {
                spans.push(Span::styled(
                    target.to_owned(),
                    semantic_style(state, SemanticColor::Assistant),
                ));
                spans.push(Span::styled(
                    " · ".to_owned(),
                    semantic_style(state, SemanticColor::Muted),
                ));
                spans.push(Span::styled(
                    query.to_owned(),
                    semantic_style(state, SemanticColor::Command).add_modifier(Modifier::BOLD),
                ));
            } else if rest.starts_with('"') {
                spans.push(Span::styled(
                    rest.to_owned(),
                    semantic_style(state, SemanticColor::Command).add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    rest.to_owned(),
                    semantic_style(state, SemanticColor::Assistant),
                ));
            }
        }
        return;
    }

    spans.extend(inline_code_spans(
        state,
        body,
        semantic_style(state, SemanticColor::Assistant),
    ));
}

fn append_mcp_arg_spans(state: &TuiState, args: &str, spans: &mut Vec<Span<'static>>) {
    let mut remaining = args;
    let mut first = true;
    while !remaining.is_empty() {
        let end = next_arg_token_end(remaining);
        let token = &remaining[..end];
        if !first {
            spans.push(Span::raw(" "));
        }
        first = false;
        if let Some((key, value)) = token.split_once('=')
            && !key.is_empty()
        {
            spans.push(Span::styled(
                format!("{key}="),
                semantic_style(state, SemanticColor::Muted),
            ));
            spans.push(Span::styled(
                value.to_owned(),
                semantic_style(state, SemanticColor::Assistant),
            ));
        } else {
            spans.push(Span::styled(
                token.to_owned(),
                semantic_style(state, SemanticColor::Assistant),
            ));
        }
        remaining = remaining[end..].trim_start();
    }
}

fn next_arg_token_end(text: &str) -> usize {
    let mut in_quotes = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            ch if ch.is_whitespace() && !in_quotes => return index,
            _ => {}
        }
    }
    text.len()
}
