//! Hyperlink metadata, bare-URL detection, security validation, browser launching,
//! and floating hover-tooltip re-exports for timeline views.

mod tooltip;

pub(crate) use tooltip::render_link_tooltip;

use super::{
    state::TuiState,
    text_wrap::{StyledTextPart, semantic_style},
    theme::SemanticColor,
};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use std::{process::Stdio, sync::Arc, time::Duration};
use thiserror::Error;
use unicode_width::UnicodeWidthStr;

/// Maximum accepted URL length when launching an external browser.
const MAX_EXTERNAL_URL_BYTES: usize = 8192;

/// Maximum time to wait for a platform browser launcher to report immediate
/// failure before detaching it cleanly.
const BROWSER_LAUNCH_WAIT: Duration = Duration::from_secs(2);

/// Hyperlink hit span on a single wrapped line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineLink {
    pub(crate) column: u16,
    pub(crate) width: u16,
    pub(crate) url: String,
}

impl LineLink {
    pub(crate) fn new(column: u16, width: u16, url: String) -> Self {
        Self { column, width, url }
    }
}

/// Clickable and hoverable hyperlink target anchored to a logical timeline row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkTarget {
    pub(crate) line_index: usize,
    pub(crate) column: u16,
    pub(crate) width: u16,
    pub(crate) url: String,
}

impl LinkTarget {
    pub(crate) fn new(line_index: usize, column: u16, width: u16, url: String) -> Self {
        Self {
            line_index,
            column,
            width,
            url,
        }
    }
}

/// Currently hovered hyperlink inside a timeline pane, in document coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HoveredLink {
    pub(crate) url: String,
    /// Horizontal offset relative to the timeline body's left edge.
    pub(crate) column: u16,
    pub(crate) width: u16,
    pub(crate) document_row: usize,
    pub(crate) item_index: usize,
}

impl HoveredLink {
    pub(crate) fn new(
        url: String,
        column: u16,
        width: u16,
        document_row: usize,
        item_index: usize,
    ) -> Self {
        Self {
            url,
            column,
            width,
            document_row,
            item_index,
        }
    }
}

/// Shared inline hyperlink style across Markdown, tables, user messages, and diagnostics.
pub(crate) fn link_style(state: &TuiState) -> Style {
    semantic_style(state, SemanticColor::Link).add_modifier(Modifier::UNDERLINED)
}

/// Returns a validated link target when inline code contains a standalone `http(s)` URL.
pub(crate) fn inline_code_link(code: &str) -> Option<Arc<str>> {
    validate_external_url(code.trim()).ok().map(Arc::from)
}

/// Appends plain text into `parts`, promoting bare `http://`/`https://` URLs
/// into styled, clickable hyperlink parts while preserving `base_style.bg`.
pub(crate) fn push_text_with_bare_urls(
    parts: &mut Vec<StyledTextPart>,
    state: &TuiState,
    text: &str,
    base_style: Style,
) {
    let url_style = match base_style.bg {
        Some(bg) => link_style(state).bg(bg),
        None => link_style(state),
    };
    for segment in split_bare_urls(text) {
        match segment {
            TextOrUrl::Text(chunk) => {
                parts.push(StyledTextPart::new(chunk.to_owned(), base_style, false));
            }
            TextOrUrl::Url(url) => {
                parts.push(StyledTextPart::with_link(
                    url.to_owned(),
                    url_style,
                    false,
                    Some(Arc::from(url)),
                ));
            }
        }
    }
}

/// Scans a single pre-wrapped display line for bare `http://`/`https://` URLs
/// not already covered by `existing_targets`, underlines those URL spans in
/// place, and returns newly discovered [`LinkTarget`]s for `line_index`.
pub(crate) fn linkify_rendered_line(
    line: &mut Line<'static>,
    line_index: usize,
    existing_targets: &[LinkTarget],
) -> Vec<LinkTarget> {
    let has_colon = line.spans.iter().any(|span| span.content.contains(':'));
    let has_slashes = line.spans.iter().any(|span| span.content.contains("//"));
    if !has_colon || !has_slashes {
        return Vec::new();
    }

    let full_text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    if !full_text.contains("://") {
        return Vec::new();
    }

    let mut byte_offset = 0usize;
    let mut col_offset = 0usize;
    let mut url_ranges: Vec<(usize, usize)> = Vec::new();
    let mut discovered: Vec<LinkTarget> = Vec::new();

    for segment in split_bare_urls(&full_text) {
        match segment {
            TextOrUrl::Text(chunk) => {
                byte_offset += chunk.len();
                col_offset += UnicodeWidthStr::width(chunk);
            }
            TextOrUrl::Url(url) => {
                let start_byte = byte_offset;
                let end_byte = byte_offset + url.len();
                let start_col = u16::try_from(col_offset).unwrap_or(u16::MAX);
                let width = u16::try_from(UnicodeWidthStr::width(url)).unwrap_or(u16::MAX);
                let end_col = start_col.saturating_add(width);

                let overlaps_existing = existing_targets.iter().any(|target| {
                    target.line_index == line_index
                        && target.column < end_col
                        && target.column.saturating_add(target.width) > start_col
                });
                if !overlaps_existing && width > 0 {
                    url_ranges.push((start_byte, end_byte));
                    discovered.push(LinkTarget::new(
                        line_index,
                        start_col,
                        width,
                        url.to_owned(),
                    ));
                }

                byte_offset = end_byte;
                col_offset += UnicodeWidthStr::width(url);
            }
        }
    }

    if !url_ranges.is_empty() {
        underline_byte_ranges(line, &url_ranges);
    }
    discovered
}

fn underline_byte_ranges(line: &mut Line<'static>, ranges: &[(usize, usize)]) {
    let old_spans = std::mem::take(&mut line.spans);
    let mut new_spans = Vec::with_capacity(old_spans.len() + ranges.len() * 2);
    let mut cursor = 0usize;

    for span in old_spans {
        let span_start = cursor;
        let span_end = span_start + span.content.len();
        cursor = span_end;

        let content = span.content.as_ref();
        let mut local_pos = 0usize;
        for &(range_start, range_end) in ranges {
            if range_end <= span_start {
                continue;
            }
            if range_start >= span_end {
                break;
            }
            let overlap_start = range_start.max(span_start) - span_start;
            let overlap_end = range_end.min(span_end) - span_start;
            if overlap_start > local_pos {
                new_spans.push(Span::styled(
                    content[local_pos..overlap_start].to_owned(),
                    span.style,
                ));
            }
            if overlap_end > overlap_start {
                new_spans.push(Span::styled(
                    content[overlap_start..overlap_end].to_owned(),
                    span.style.add_modifier(Modifier::UNDERLINED),
                ));
            }
            local_pos = overlap_end;
        }
        if local_pos < content.len() {
            new_spans.push(Span::styled(content[local_pos..].to_owned(), span.style));
        }
    }

    line.spans = new_spans;
}

/// Failure reason when validating a hyperlink before opening it externally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum UrlValidationError {
    #[error("link URL is empty")]
    Empty,
    #[error("link URL exceeds maximum supported length")]
    TooLong,
    #[error("link URL contains whitespace or disallowed control characters")]
    InvalidCharacter,
    #[error("only http:// and https:// links can be opened in a browser")]
    UnsupportedScheme,
    #[error("link URL is missing a valid host")]
    MissingHost,
}

/// Segment produced when scanning plain text for bare `http://` / `https://` URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextOrUrl<'a> {
    Text(&'a str),
    Url(&'a str),
}

/// Validates that `url` is a safe, well-formed `http://` or `https://` URL
/// suitable for passing as an explicit argument to a platform browser launcher.
pub(crate) fn validate_external_url(url: &str) -> Result<&str, UrlValidationError> {
    if url.is_empty() {
        return Err(UrlValidationError::Empty);
    }
    if url.len() > MAX_EXTERNAL_URL_BYTES {
        return Err(UrlValidationError::TooLong);
    }
    if url.chars().any(|ch| {
        ch.is_control() || ch.is_whitespace() || matches!(ch, '"' | '<' | '>' | '`' | '\\')
    }) {
        return Err(UrlValidationError::InvalidCharacter);
    }
    let after_scheme = strip_http_scheme(url).ok_or(UrlValidationError::UnsupportedScheme)?;
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() {
        return Err(UrlValidationError::MissingHost);
    }
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return Err(UrlValidationError::MissingHost);
        };
        &rest[..end]
    } else {
        host_port
            .split_once(':')
            .map_or(host_port, |(host, _)| host)
    };
    if host.is_empty() || host.starts_with('.') || host.starts_with('-') {
        return Err(UrlValidationError::MissingHost);
    }
    Ok(url)
}

pub(super) fn strip_http_scheme(url: &str) -> Option<&str> {
    if let Some(prefix) = url.get(..8)
        && prefix.eq_ignore_ascii_case("https://")
    {
        return url.get(8..);
    }
    if let Some(prefix) = url.get(..7)
        && prefix.eq_ignore_ascii_case("http://")
    {
        return url.get(7..);
    }
    None
}

/// Splits plain text into literal text slices and validated bare `http://`/`https://` URLs.
pub(crate) fn split_bare_urls(text: &str) -> Vec<TextOrUrl<'_>> {
    if !text.contains("://") {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![TextOrUrl::Text(text)]
        };
    }

    let mut segments = Vec::new();
    let mut cursor = 0usize;
    let mut scan = 0usize;

    while scan < text.len() {
        let Some(rel_sep) = text[scan..].find("://") else {
            break;
        };
        let sep_pos = scan + rel_sep;
        let scheme_start = if sep_pos >= 5
            && text.is_char_boundary(sep_pos - 5)
            && text[sep_pos - 5..sep_pos].eq_ignore_ascii_case("https")
        {
            Some(sep_pos - 5)
        } else if sep_pos >= 4
            && text.is_char_boundary(sep_pos - 4)
            && text[sep_pos - 4..sep_pos].eq_ignore_ascii_case("http")
        {
            Some(sep_pos - 4)
        } else {
            None
        };

        let Some(start) = scheme_start else {
            scan = sep_pos + 3;
            continue;
        };

        let valid_boundary = start == 0
            || text[..start]
                .chars()
                .next_back()
                .is_none_or(|prev| !prev.is_ascii_alphanumeric() && prev != '_' && prev != '/');
        if !valid_boundary {
            scan = sep_pos + 3;
            continue;
        }

        let mut raw_end = text.len();
        for (offset, ch) in text[start..].char_indices() {
            if is_bare_url_delimiter(ch) {
                raw_end = start + offset;
                break;
            }
        }

        let trimmed_end = trim_bare_url_end(&text[start..raw_end]) + start;
        let candidate = &text[start..trimmed_end];
        if validate_external_url(candidate).is_ok() {
            if start > cursor {
                segments.push(TextOrUrl::Text(&text[cursor..start]));
            }
            segments.push(TextOrUrl::Url(candidate));
            cursor = trimmed_end;
            scan = trimmed_end;
        } else {
            scan = sep_pos + 3;
        }
    }

    if cursor < text.len() {
        segments.push(TextOrUrl::Text(&text[cursor..]));
    }
    segments
}

fn is_bare_url_delimiter(ch: char) -> bool {
    ch.is_whitespace()
        || ch.is_control()
        || matches!(
            ch,
            '<' | '>'
                | '"'
                | '`'
                | '\\'
                | '，'
                | '。'
                | '；'
                | '：'
                | '！'
                | '？'
                | '、'
                | '（'
                | '）'
                | '【'
                | '】'
                | '《'
                | '》'
                | '「'
                | '」'
                | '『'
                | '』'
                | '“'
                | '”'
                | '‘'
                | '’'
        )
}

fn trim_bare_url_end(candidate: &str) -> usize {
    let mut end = candidate.len();
    loop {
        let current = &candidate[..end];
        let Some(last) = current.chars().next_back() else {
            break;
        };
        let should_strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | ']' | '}' => true,
            ')' => {
                let open = current.chars().filter(|&c| c == '(').count();
                let close = current.chars().filter(|&c| c == ')').count();
                close > open
            }
            _ => false,
        };
        if should_strip {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Opens a validated external `http://` or `https://` URL using the platform's
/// browser launcher without invoking a shell.
pub(crate) async fn open_external_url(url: &str) -> Result<(), String> {
    let validated = validate_external_url(url).map_err(|error| error.to_string())?;
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![validated])
    } else if cfg!(target_os = "linux") {
        ("xdg-open", vec![validated])
    } else if cfg!(target_os = "windows") {
        // Invokes the URL handler directly without cmd.exe so query characters
        // like `&` are never interpreted as shell command separators.
        (
            "rundll32.exe",
            vec!["url.dll,FileProtocolHandler", validated],
        )
    } else {
        return Err("automatic browser opening is unsupported on this platform".to_owned());
    };

    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("browser launcher {program} failed: {error}"))?;

    match tokio::time::timeout(BROWSER_LAUNCH_WAIT, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(format!("browser launcher {program} exited with {status}")),
        Ok(Err(error)) => Err(format!("browser launcher {program} failed: {error}")),
        // Some Linux launchers keep the child alive while the spawned browser
        // runs; treat surviving past the startup window as a successful launch.
        Err(_) => Ok(()),
    }
}

/// Formats a compact user-facing feedback notice after opening a URL.
pub(crate) fn open_url_feedback(url: &str, result: Result<(), String>) -> (String, bool) {
    let clean = crate::text::without_control_chars(url);
    match result {
        Ok(()) => (format!("Opened {clean}"), false),
        Err(error) => (format!("Could not open link: {error}"), true),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LinkTarget, TextOrUrl, UrlValidationError, linkify_rendered_line, split_bare_urls,
        validate_external_url,
    };
    use ratatui::{
        style::{Modifier, Style},
        text::{Line, Span},
    };

    #[test]
    fn validate_external_url_accepts_safe_http_and_https_urls() {
        for url in [
            "https://openai.com",
            "https://docs.rs/ratatui/latest/?a=1&b=2#top",
            "http://127.0.0.1:8080/session/abc",
            "http://localhost:3000/foo",
            "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            "https://www.youtube.com/@handle",
            "HTTP://EXAMPLE.COM/UPPER",
            "https://[::1]:8080/health",
        ] {
            assert_eq!(
                validate_external_url(url),
                Ok(url),
                "expected safe URL to pass: {url}"
            );
        }
    }

    #[test]
    fn validate_external_url_rejects_unsafe_schemes_control_chars_and_missing_hosts() {
        assert_eq!(validate_external_url(""), Err(UrlValidationError::Empty));
        assert_eq!(
            validate_external_url("javascript:alert(1)"),
            Err(UrlValidationError::UnsupportedScheme)
        );
        assert_eq!(
            validate_external_url("file:///etc/passwd"),
            Err(UrlValidationError::UnsupportedScheme)
        );
        assert_eq!(
            validate_external_url("data:text/html,<script>alert(1)</script>"),
            Err(UrlValidationError::InvalidCharacter)
        );
        assert_eq!(
            validate_external_url("mailto:user@example.com"),
            Err(UrlValidationError::UnsupportedScheme)
        );
        assert_eq!(
            validate_external_url("./relative/path.md"),
            Err(UrlValidationError::UnsupportedScheme)
        );
        assert_eq!(
            validate_external_url("--flag"),
            Err(UrlValidationError::UnsupportedScheme)
        );
        assert_eq!(
            validate_external_url("https://"),
            Err(UrlValidationError::MissingHost)
        );
        assert_eq!(
            validate_external_url("https:///only-path"),
            Err(UrlValidationError::MissingHost)
        );
        assert_eq!(
            validate_external_url("https://example.com/a\nb"),
            Err(UrlValidationError::InvalidCharacter)
        );
        assert_eq!(
            validate_external_url("https://example.com/a b"),
            Err(UrlValidationError::InvalidCharacter)
        );
        assert_eq!(
            validate_external_url("https://example.com/\"quoted\""),
            Err(UrlValidationError::InvalidCharacter)
        );
        let oversized = format!("https://example.com/{}", "a".repeat(9000));
        assert_eq!(
            validate_external_url(&oversized),
            Err(UrlValidationError::TooLong)
        );
    }

    #[test]
    fn split_bare_urls_handles_punctuation_balanced_parens_and_cjk_neighbors() {
        assert_eq!(
            split_bare_urls(
                "See (https://en.wikipedia.org/wiki/Rust_(lang)), and https://example.com/docs."
            ),
            vec![
                TextOrUrl::Text("See ("),
                TextOrUrl::Url("https://en.wikipedia.org/wiki/Rust_(lang)"),
                TextOrUrl::Text("), and "),
                TextOrUrl::Url("https://example.com/docs"),
                TextOrUrl::Text("."),
            ]
        );
        assert_eq!(
            split_bare_urls("参考https://example.com/cn，更多见http://localhost:8080/a。"),
            vec![
                TextOrUrl::Text("参考"),
                TextOrUrl::Url("https://example.com/cn"),
                TextOrUrl::Text("，更多见"),
                TextOrUrl::Url("http://localhost:8080/a"),
                TextOrUrl::Text("。"),
            ]
        );
        assert_eq!(
            split_bare_urls("not-a-boundaryhttps://example.com"),
            vec![TextOrUrl::Text("not-a-boundaryhttps://example.com")]
        );
    }

    #[test]
    fn linkify_rendered_line_handles_split_spans_and_preserves_existing_links() {
        // Syntax-highlighted spans may split scheme and host across separate spans.
        let mut line = Line::from(vec![
            Span::styled("▎ curl ".to_owned(), Style::default()),
            Span::styled("Https".to_owned(), Style::default()),
            Span::styled(":".to_owned(), Style::default()),
            Span::styled("//example.com/a_b?x=1&y=2".to_owned(), Style::default()),
        ]);
        let found = linkify_rendered_line(&mut line, 3, &[]);
        assert_eq!(
            found,
            vec![LinkTarget::new(
                3,
                7,
                31,
                "Https://example.com/a_b?x=1&y=2".to_owned()
            )]
        );
        assert!(line.spans.iter().any(|span| {
            span.content == "Https" && span.style.add_modifier.contains(Modifier::UNDERLINED)
        }));

        // Existing Markdown link target (e.g. anti-phishing masked link) is never overwritten.
        let mut masked = Line::from(Span::raw("https://trusted.example".to_owned()));
        let existing = vec![LinkTarget::new(
            0,
            0,
            23,
            "https://evil.example/phish".to_owned(),
        )];
        assert!(linkify_rendered_line(&mut masked, 0, &existing).is_empty());
    }

    #[test]
    fn strip_http_scheme_and_inline_code_link_never_panic_on_multibyte_utf8() {
        for sample in ["【完】", "表格测试", "🙂🙂", "a【完】", "ab【完】"] {
            assert_eq!(super::strip_http_scheme(sample), None);
            assert_eq!(super::inline_code_link(sample), None);
            assert!(matches!(
                validate_external_url(sample),
                Err(UrlValidationError::UnsupportedScheme)
            ));
        }
        assert_eq!(
            super::strip_http_scheme("HTTPS://example.com/路径"),
            Some("example.com/路径")
        );
        assert_eq!(
            super::strip_http_scheme("Http://example.com/路径"),
            Some("example.com/路径")
        );
    }
}
