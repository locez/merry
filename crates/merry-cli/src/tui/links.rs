//! Hyperlink metadata, bare-URL detection, security validation, browser launching,
//! and floating hover-tooltip rendering for timeline views.

use super::{
    state::TuiState,
    text_wrap::{StyledTextPart, semantic_style},
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use std::{process::Stdio, sync::Arc, time::Duration};
use thiserror::Error;
use unicode_width::UnicodeWidthChar;

/// Maximum accepted URL length when launching an external browser.
const MAX_EXTERNAL_URL_BYTES: usize = 8192;

/// Maximum time to wait for a platform browser launcher to report immediate
/// failure before detaching it cleanly.
const BROWSER_LAUNCH_WAIT: Duration = Duration::from_secs(2);

/// Maximum inner text lines displayed inside a floating link hover tooltip.
const MAX_TOOLTIP_LINES: usize = 3;

/// Maximum inner text width of a floating link hover tooltip.
const MAX_TOOLTIP_INNER_WIDTH: u16 = 84;

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

/// Shared inline hyperlink style across Markdown, tables, and diagnostics.
pub(crate) fn link_style(state: &TuiState) -> Style {
    semantic_style(state, SemanticColor::Link).add_modifier(Modifier::UNDERLINED)
}

/// Appends plain text into `parts`, promoting bare `http://`/`https://` URLs
/// into styled, clickable hyperlink parts.
pub(crate) fn push_text_with_bare_urls(
    parts: &mut Vec<StyledTextPart>,
    state: &TuiState,
    text: &str,
    base_style: Style,
) {
    for segment in split_bare_urls(text) {
        match segment {
            TextOrUrl::Text(chunk) => {
                parts.push(StyledTextPart::new(chunk.to_owned(), base_style, false));
            }
            TextOrUrl::Url(url) => {
                parts.push(StyledTextPart::with_link(
                    url.to_owned(),
                    link_style(state),
                    false,
                    Some(Arc::from(url)),
                ));
            }
        }
    }
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

fn strip_http_scheme(url: &str) -> Option<&str> {
    if url.len() >= 8 && url[..8].eq_ignore_ascii_case("https://") {
        Some(&url[8..])
    } else if url.len() >= 7 && url[..7].eq_ignore_ascii_case("http://") {
        Some(&url[7..])
    } else {
        None
    }
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

/// Highlights the hovered hyperlink segment and draws a floating anti-phishing
/// URL preview tooltip anchored above (or below) the link within `bounds`.
pub(crate) fn render_link_tooltip(
    frame: &mut Frame<'_>,
    state: &TuiState,
    bounds: Rect,
    body: Rect,
    hovered: &HoveredLink,
    screen_y: u16,
) {
    if bounds.is_empty() || body.is_empty() {
        return;
    }

    let anchor_x = body.x.saturating_add(hovered.column);

    // Highlight the hovered link segment so the user sees the exact active span.
    if screen_y >= body.y && screen_y < body.bottom() {
        let start_x = anchor_x.max(body.x);
        let end_x = anchor_x.saturating_add(hovered.width).min(body.right());
        let buf = frame.buffer_mut();
        for x in start_x..end_x {
            let cell = &mut buf[(x, screen_y)];
            cell.set_style(
                cell.style()
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            );
        }
    }

    let clean_url = crate::text::without_control_chars(&hovered.url);
    let openable = validate_external_url(&hovered.url).is_ok();
    let surface = tooltip_surface_style(state);

    let max_inner_width = bounds
        .width
        .saturating_sub(4)
        .clamp(1, MAX_TOOLTIP_INNER_WIDTH);
    let wrapped_lines = build_tooltip_lines(state, &clean_url, openable, surface, max_inner_width);
    if wrapped_lines.is_empty() {
        return;
    }

    let content_width = wrapped_lines.iter().map(Line::width).max().unwrap_or(1);
    let content_lines = u16::try_from(wrapped_lines.len()).unwrap_or(1);

    let can_draw_boxed = bounds.width >= 6 && bounds.height >= content_lines.saturating_add(3);

    if can_draw_boxed {
        // Box: `│ ` + content + ` │` => content_width + 4.
        let box_width = u16::try_from(content_width)
            .unwrap_or(bounds.width)
            .saturating_add(4)
            .min(bounds.width);
        let box_height = content_lines.saturating_add(2);
        let tooltip_rect = place_tooltip_rect(bounds, anchor_x, screen_y, box_width, box_height);

        let border_slot = if openable {
            SemanticColor::BorderFocus
        } else {
            SemanticColor::Warning
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(semantic_style(state, border_slot).patch(surface))
            .style(surface);

        frame.render_widget(Clear, tooltip_rect);
        let inner = block.inner(tooltip_rect);
        frame.render_widget(block, tooltip_rect);
        if !inner.is_empty() {
            let padded_lines: Vec<Line<'static>> = wrapped_lines
                .into_iter()
                .map(|mut line| {
                    line.spans.insert(0, Span::styled(" ", surface));
                    line
                })
                .collect();
            frame.render_widget(Paragraph::new(padded_lines).style(surface), inner);
        }
        return;
    }

    // Compact 1-row fallback when the pane is too short for a bordered card.
    if bounds.height < 2 {
        return;
    }
    let bar_width = u16::try_from(content_width)
        .unwrap_or(bounds.width)
        .saturating_add(2)
        .min(bounds.width);
    let tooltip_rect = place_tooltip_rect(bounds, anchor_x, screen_y, bar_width, 1);
    frame.render_widget(Clear, tooltip_rect);
    let mut line = wrapped_lines.into_iter().next().unwrap_or_default();
    line.spans.insert(0, Span::styled(" ", surface));
    frame.render_widget(Paragraph::new(line).style(surface), tooltip_rect);
}

fn place_tooltip_rect(bounds: Rect, anchor_x: u16, anchor_y: u16, width: u16, height: u16) -> Rect {
    let width = width.min(bounds.width).max(1);
    let height = height.min(bounds.height).max(1);

    let max_x = bounds.right().saturating_sub(width);
    let x = anchor_x.max(bounds.x).min(max_x);

    let above_space = anchor_y.saturating_sub(bounds.y);
    let below_start = anchor_y.saturating_add(1);
    let below_space = bounds.bottom().saturating_sub(below_start);

    let y = if above_space >= height {
        anchor_y.saturating_sub(height)
    } else if below_space >= height {
        below_start
    } else if above_space >= below_space {
        bounds.y
    } else {
        bounds.bottom().saturating_sub(height)
    };

    Rect::new(x, y, width, height)
}

fn tooltip_surface_style(state: &TuiState) -> Style {
    state
        .theme()
        .bg_color(SemanticColor::Surface)
        .or_else(|| state.theme().bg_color(SemanticColor::SurfaceSubtle))
        .or_else(|| state.theme().bg_color(SemanticColor::HeaderBackground))
        .map_or_else(Style::default, |bg| Style::default().bg(bg))
}

fn build_tooltip_lines(
    state: &TuiState,
    clean_url: &str,
    openable: bool,
    surface: Style,
    max_width: u16,
) -> Vec<Line<'static>> {
    let max_width = usize::from(max_width).max(1);
    let mut styled_chars: Vec<(char, Style)> = Vec::new();

    if openable {
        let icon_style = semantic_style(state, SemanticColor::Status)
            .add_modifier(Modifier::BOLD)
            .patch(surface);
        let host_style = semantic_style(state, SemanticColor::Link)
            .add_modifier(Modifier::BOLD)
            .patch(surface);
        let rest_style = semantic_style(state, SemanticColor::TextSecondary).patch(surface);

        let split_at = split_scheme_and_authority(clean_url);
        styled_chars.extend("↗ ".chars().map(|ch| (ch, icon_style)));
        styled_chars.extend(clean_url[..split_at].chars().map(|ch| (ch, host_style)));
        styled_chars.extend(clean_url[split_at..].chars().map(|ch| (ch, rest_style)));
    } else {
        let warn_style = semantic_style(state, SemanticColor::Warning)
            .add_modifier(Modifier::BOLD)
            .patch(surface);
        let muted_style = semantic_style(state, SemanticColor::Muted).patch(surface);
        styled_chars.extend("⊘ ".chars().map(|ch| (ch, warn_style)));
        styled_chars.extend(clean_url.chars().map(|ch| (ch, warn_style)));
        styled_chars.extend(" (not http/https)".chars().map(|ch| (ch, muted_style)));
    }

    wrap_styled_chars(styled_chars, max_width, MAX_TOOLTIP_LINES, surface)
}

fn split_scheme_and_authority(url: &str) -> usize {
    let Some(after_scheme) = strip_http_scheme(url) else {
        return url.len();
    };
    let scheme_len = url.len() - after_scheme.len();
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    scheme_len + authority_end
}

fn wrap_styled_chars(
    chars: Vec<(char, Style)>,
    max_width: usize,
    max_lines: usize,
    fallback_style: Style,
) -> Vec<Line<'static>> {
    if chars.is_empty() || max_lines == 0 {
        return Vec::new();
    }

    let mut lines: Vec<Vec<(char, Style)>> = vec![Vec::new()];
    let mut current_width = 0usize;
    let mut truncated = false;

    for (ch, style) in chars {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width > 0 && current_width + ch_width > max_width {
            if lines.len() >= max_lines {
                truncated = true;
                break;
            }
            lines.push(Vec::new());
            current_width = 0;
        }
        if let Some(last) = lines.last_mut() {
            last.push((ch, style));
        }
        current_width += ch_width;
    }

    if truncated && let Some(last) = lines.last_mut() {
        let mut width: usize = last
            .iter()
            .map(|(ch, _)| UnicodeWidthChar::width(*ch).unwrap_or(0))
            .sum();
        while !last.is_empty() && width + 1 > max_width {
            if let Some((removed, _)) = last.pop() {
                width = width.saturating_sub(UnicodeWidthChar::width(removed).unwrap_or(0));
            }
        }
        let ellipsis_style = last.last().map_or(fallback_style, |(_, s)| *s);
        last.push(('…', ellipsis_style));
    }

    lines
        .into_iter()
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut buf = String::new();
            let mut active_style: Option<Style> = None;
            for (ch, style) in row {
                if active_style == Some(style) {
                    buf.push(ch);
                } else {
                    if let Some(prev_style) = active_style.take()
                        && !buf.is_empty()
                    {
                        spans.push(Span::styled(std::mem::take(&mut buf), prev_style));
                    }
                    buf.push(ch);
                    active_style = Some(style);
                }
            }
            if let Some(prev_style) = active_style
                && !buf.is_empty()
            {
                spans.push(Span::styled(buf, prev_style));
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        TextOrUrl, UrlValidationError, place_tooltip_rect, split_bare_urls, validate_external_url,
    };
    use ratatui::layout::Rect;

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
    fn place_tooltip_rect_prefers_above_and_flips_below_when_near_top() {
        let bounds = Rect::new(2, 1, 80, 20);
        // Plenty of room above y=10 -> places above without covering y=10.
        let above = place_tooltip_rect(bounds, 70, 10, 30, 3);
        assert_eq!(above.y, 7);
        assert_eq!(above.bottom(), 10);
        assert_eq!(above.right(), bounds.right());

        // Near top y=2 -> only 1 row above, flips directly below to y=3.
        let below = place_tooltip_rect(bounds, 5, 2, 30, 3);
        assert_eq!(below.y, 3);
        assert_eq!(below.x, 5);
    }
}
