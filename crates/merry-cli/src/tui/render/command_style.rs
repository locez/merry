//! Display-only shell token styling; never parses or authorizes execution.

use crate::tui::{state::TuiState, text_wrap::semantic_style, theme::SemanticColor};
use ratatui::{style::Modifier, text::Span};

pub(super) fn command_spans(state: &TuiState, command: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut offset = 0;
    let mut expects_command = true;

    while offset < command.len() {
        let rest = &command[offset..];
        let Some(first) = rest.chars().next() else {
            break;
        };

        if first.is_whitespace() {
            let end = rest
                .char_indices()
                .find_map(|(index, character)| (!character.is_whitespace()).then_some(index))
                .unwrap_or(rest.len());
            spans.push(Span::raw(rest[..end].to_owned()));
            offset += end;
            continue;
        }

        if let Some(end) = shell_operator_len(rest) {
            spans.push(Span::styled(
                rest[..end].to_owned(),
                semantic_style(state, SemanticColor::ToolKeyword),
            ));
            expects_command = shell_operator_starts_command(&rest[..end]);
            offset += end;
            continue;
        }

        if first == '\'' || first == '"' {
            let end = shell_quoted_token_len(rest, first);
            spans.push(Span::styled(
                rest[..end].to_owned(),
                semantic_style(state, SemanticColor::Command),
            ));
            expects_command = false;
            offset += end;
            continue;
        }

        let end = shell_word_len(rest);
        let word = &rest[..end];
        let color = if expects_command {
            SemanticColor::Command
        } else if word.starts_with('-') {
            SemanticColor::Focus
        } else if word.starts_with('$') {
            SemanticColor::Success
        } else {
            SemanticColor::Assistant
        };
        let mut style = semantic_style(state, color);
        if expects_command {
            style = style.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(word.to_owned(), style));
        expects_command = false;
        offset += end;
    }

    spans
}

pub(super) fn shell_operator_len(text: &str) -> Option<usize> {
    [
        "2>&1", "2>>", "2>", "&&", "||", ">>", "<<", ">", "<", "|", ";", "&",
    ]
    .iter()
    .find_map(|operator| text.starts_with(operator).then_some(operator.len()))
}

pub(super) fn shell_operator_starts_command(operator: &str) -> bool {
    matches!(operator, "|" | "||" | "&&" | ";" | "&")
}

pub(super) fn shell_quoted_token_len(text: &str, quote: char) -> usize {
    let mut escaped = false;
    for (index, character) in text.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if character == '\\' && quote == '"' {
            escaped = true;
        } else if character == quote {
            return index + character.len_utf8();
        }
    }
    text.len()
}

pub(super) fn shell_word_len(text: &str) -> usize {
    text.char_indices()
        .find_map(|(index, character)| {
            (character.is_whitespace()
                || shell_operator_len(&text[index..]).is_some()
                || character == '\''
                || character == '"')
                .then_some(index)
        })
        .unwrap_or(text.len())
}
