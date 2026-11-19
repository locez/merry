//! Command and output highlighting powered by `syntect`/`two-face` and the ANSI SGR parser.

use super::ansi::append_ansi_output;
use crate::tui::{
    highlight::{detect_syntax_from_first_line, has_syntax, highlight_code_to_lines},
    state::TuiState,
    text_wrap::{StyledTextPart, semantic_style, wrap_styled_parts_preserving_leading_whitespace},
    theme::SemanticColor,
};
use ratatui::{
    style::{Modifier, Style},
    text::Line,
};
use std::borrow::Cow;

const MAX_COMMAND_HIGHLIGHT_BYTES: usize = 8 * 1024;
const MAX_COMMAND_HIGHLIGHT_LINES: usize = 100;
const MAX_SYNTECT_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_SYNTECT_OUTPUT_LINES: usize = 2_500;

/// Highlights a shell command with a `$ ` prompt prefix and wraps it to `width`.
pub(super) fn highlight_command_lines(
    state: &TuiState,
    command: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let prompt_style =
        semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD);
    let fallback_style = semantic_style(state, SemanticColor::Command).add_modifier(Modifier::BOLD);
    let raw_lines: Vec<Vec<StyledTextPart>> = if command.len() <= MAX_COMMAND_HIGHLIGHT_BYTES
        && command.lines().count() <= MAX_COMMAND_HIGHLIGHT_LINES
    {
        highlight_code_to_lines(command, "bash", state.code_theme())
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| StyledTextPart {
                        text: sanitize_plain_fragment(&span.content),
                        style: if span.style.fg.is_some() {
                            span.style
                        } else {
                            fallback_style
                        },
                        atomic: false,
                    })
                    .collect()
            })
            .collect()
    } else {
        command
            .split('\n')
            .map(|line| {
                vec![StyledTextPart {
                    text: sanitize_plain_fragment(line),
                    style: fallback_style,
                    atomic: false,
                }]
            })
            .collect()
    };

    let mut out = Vec::new();
    for (index, mut parts) in raw_lines.into_iter().enumerate() {
        let prefix = if index == 0 { "$ " } else { "  " };
        let mut line_parts = Vec::with_capacity(parts.len() + 1);
        line_parts.push(StyledTextPart {
            text: prefix.to_owned(),
            style: prompt_style,
            atomic: true,
        });
        line_parts.append(&mut parts);
        out.extend(wrap_styled_parts_preserving_leading_whitespace(
            line_parts, width,
        ));
    }
    if out.is_empty() {
        out.push(Line::default());
    }
    out
}

/// Highlights captured command output using ANSI SGR or `syntect`/`two-face` grammars.
pub(super) fn append_highlighted_output(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    command: &str,
    text: &str,
    width: u16,
) {
    let base_style = semantic_style(state, SemanticColor::Assistant);
    let text = normalize_carriage_returns(text);

    if text.contains('\x1b') {
        append_ansi_output(lines, &text, base_style, width);
        return;
    }

    let line_count = text.bytes().filter(|byte| *byte == b'\n').count() + 1;
    if text.len() <= MAX_SYNTECT_OUTPUT_BYTES && line_count <= MAX_SYNTECT_OUTPUT_LINES {
        let syntax = select_output_syntax(command, &text);
        append_syntect_output(lines, state, &text, syntax, base_style, width);
        return;
    }

    append_plain_output(lines, &text, base_style, width);
}

/// `SyntaxSet` resolves an extension, a language name, and a shebang or mode line.
/// Diff markers and a JSON prefix are the shapes it does not classify.
fn select_output_syntax<'a>(command: &'a str, text: &str) -> &'a str {
    if let Some(extension) = reader_extension(command) {
        return extension;
    }
    if looks_like_unified_diff(text) {
        return "diff";
    }
    if let Some(syntax) = first_line_syntax(text) {
        return syntax;
    }
    if looks_like_json_value(text) {
        return "json";
    }
    "log"
}

fn first_line_syntax(text: &str) -> Option<&'static str> {
    let line = text.lines().find(|line| !line.trim().is_empty())?;
    detect_syntax_from_first_line(line)
}

fn normalize_carriage_returns(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(character);
        }
    }
    Cow::Owned(normalized)
}

fn append_syntect_output(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    text: &str,
    syntax: &str,
    fallback_style: Style,
    width: u16,
) {
    for line in highlight_code_to_lines(text, syntax, state.code_theme()) {
        let parts = line
            .spans
            .into_iter()
            .filter_map(|span| {
                let clean = sanitize_plain_fragment(&span.content);
                (!clean.is_empty()).then_some(StyledTextPart {
                    text: clean,
                    style: if span.style.fg.is_some() {
                        span.style
                    } else {
                        fallback_style
                    },
                    atomic: false,
                })
            })
            .collect::<Vec<_>>();
        if parts.is_empty() {
            lines.push(Line::default());
        } else {
            lines.extend(wrap_styled_parts_preserving_leading_whitespace(
                parts, width,
            ));
        }
    }
}

fn append_plain_output(lines: &mut Vec<Line<'static>>, text: &str, style: Style, width: u16) {
    for raw_line in text.split('\n') {
        let clean = sanitize_plain_fragment(raw_line);
        if clean.is_empty() {
            lines.push(Line::default());
        } else {
            lines.extend(wrap_styled_parts_preserving_leading_whitespace(
                vec![StyledTextPart {
                    text: clean,
                    style,
                    atomic: false,
                }],
                width,
            ));
        }
    }
}

fn sanitize_plain_fragment(fragment: &str) -> String {
    fragment
        .chars()
        .filter(|character| !character.is_control() || *character == '\t')
        .collect::<String>()
        .replace('\t', "    ")
}

fn looks_like_unified_diff(text: &str) -> bool {
    let mut saw_minus = false;
    let mut saw_plus = false;
    for line in text.lines().take(60) {
        if line.starts_with("diff --git ") || line.starts_with("@@ -") {
            return true;
        }
        if line.starts_with("--- ") {
            saw_minus = true;
        } else if line.starts_with("+++ ") {
            saw_plus = true;
        }
        if saw_minus && saw_plus {
            return true;
        }
    }
    false
}

const JSON_SNIFF_BYTES: usize = 4 * 1024;

/// Looks at a short prefix. syntect has no JSON classifier, and a full parse
/// would build a `Value` tree only to throw it away.
fn looks_like_json_value(text: &str) -> bool {
    let bytes = text.as_bytes();
    let Some(start) = bytes.iter().position(|byte| !byte.is_ascii_whitespace()) else {
        return false;
    };
    let sample = &bytes[start..(start + JSON_SNIFF_BYTES).min(bytes.len())];
    let Some(&open) = sample.first() else {
        return false;
    };
    let mut index = 1;
    while index < sample.len() && sample[index].is_ascii_whitespace() {
        index += 1;
    }
    let Some(&next) = sample.get(index) else {
        return false;
    };
    match open {
        b'{' => next == b'"' || next == b'}',
        b'[' => next == b']' || json_array_value(sample, index),
        _ => false,
    }
}

fn json_array_value(sample: &[u8], index: usize) -> bool {
    match sample[index] {
        b'"' | b'{' | b'[' | b'-' => true,
        byte if byte.is_ascii_digit() => true,
        b't' => sample[index..].starts_with(b"true"),
        b'f' => sample[index..].starts_with(b"false"),
        b'n' => sample[index..].starts_with(b"null"),
        _ => false,
    }
}

fn reader_extension(command: &str) -> Option<&str> {
    let unwrapped = unwrap_shell_wrapper(command.lines().next()?.trim());
    let first_stage = unwrapped.split('|').next()?.trim();
    let mut tokens = first_stage.split_whitespace();
    let head = tokens.next()?;
    let rest: Vec<&str> = tokens.collect();
    let is_reader = matches!(
        head,
        "cat" | "head" | "tail" | "sed" | "nl" | "bat" | "less" | "more"
    ) || (head == "git" && rest.first() == Some(&"show"));
    if !is_reader {
        return None;
    }
    rest.iter()
        .rev()
        .find_map(|token| extension_of_path_token(token))
}

fn extension_of_path_token(token: &str) -> Option<&str> {
    let stripped = token.trim_matches(|ch| matches!(ch, '\'' | '"'));
    if stripped.is_empty() || stripped.starts_with('-') {
        return None;
    }
    let path = strip_line_column(stripped.trim_end_matches(':'));
    let (_, extension) = path.rsplit_once('.')?;
    if extension.is_empty()
        || extension.contains('/')
        || extension.contains('\\')
        || extension.contains(':')
    {
        return None;
    }
    has_syntax(extension).then_some(extension)
}

/// A trailing `:line` or `:line:col` is a compiler location, not part of the path.
/// A non-numeric suffix stays, as in `HEAD:src/lib.rs`.
fn strip_line_column(token: &str) -> &str {
    let mut path = token;
    while let Some((head, tail)) = path.rsplit_once(':') {
        if tail.is_empty() || !tail.bytes().all(|byte| byte.is_ascii_digit()) {
            break;
        }
        path = head;
    }
    path
}

fn unwrap_shell_wrapper(command: &str) -> &str {
    for prefix in [
        "bash -lc ",
        "bash -c ",
        "sh -lc ",
        "sh -c ",
        "zsh -lc ",
        "zsh -c ",
    ] {
        if let Some(rest) = command.strip_prefix(prefix) {
            let trimmed = rest.trim();
            return trimmed
                .strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
                .or_else(|| {
                    trimmed
                        .strip_prefix('"')
                        .and_then(|inner| inner.strip_suffix('"'))
                })
                .unwrap_or(trimmed);
        }
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_paths_keep_the_extension_after_a_compiler_location() {
        assert_eq!(
            select_output_syntax("cat src/lib.rs:42", "fn main() {}"),
            "rs"
        );
        assert_eq!(select_output_syntax("cat src/lib.rs:10:4", ""), "rs");
        assert_eq!(select_output_syntax("git show HEAD:src/lib.rs", ""), "rs");
        assert_eq!(
            select_output_syntax("bash -lc 'cat src/lib.rs | head -n 5'", ""),
            "rs"
        );
        assert_eq!(select_output_syntax("head -n 20 src/main.rs", ""), "rs");
    }

    #[test]
    fn diff_markers_and_a_json_prefix_pick_syntect_tokens() {
        assert_eq!(
            select_output_syntax(
                "git diff",
                "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-old\n+new"
            ),
            "diff"
        );
        assert_eq!(
            select_output_syntax("curl https://example.test", "{\n  \"enabled\": true"),
            "json"
        );
        assert_eq!(
            select_output_syntax("curl https://example.test", "[INFO] ready\nplain"),
            "log"
        );
    }

    #[test]
    fn shebang_uses_the_syntax_set_first_line_match() {
        let syntax = select_output_syntax("python3 script.py", "#!/usr/bin/env python\nprint(1)\n");
        assert_ne!(syntax, "log");
        assert_ne!(syntax, "json");
    }

    #[test]
    fn carriage_return_becomes_its_own_line() {
        assert_eq!(
            normalize_carriage_returns("10%\r20%\r\n30%"),
            "10%\n20%\n30%"
        );
        assert_eq!(
            normalize_carriage_returns("already\nwrapped"),
            "already\nwrapped"
        );
    }
}
