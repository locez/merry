//! Timeline content, wrapping, and bounded viewport projection.

use crate::tui::{
    markdown::markdown_lines,
    render::command_style::command_spans,
    state::{PatchChangeView, TimelineItem, TuiState},
    text_wrap::{
        StyledTextPart, inline_code_spans, semantic_style, truncate_chars, wrap_styled_parts,
    },
    theme::SemanticColor,
};
use merry_core::QueuedInputLane;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

// Keep prefix eviction below the viewport start when Paragraph scroll exceeds u16.
pub(super) const MAX_TIMELINE_LOGICAL_LINE_GRAPHEMES: usize = 32_768;

pub(super) const TOOL_RESULT_PREVIEW_MAX_LINES: usize = 5;

pub(super) fn render_timeline_pane(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    let timeline = timeline_lines_compact(state, region);
    let viewport = timeline_viewport(state, timeline, region);
    frame.render_widget(
        Paragraph::new(viewport.lines)
            .wrap(Wrap { trim: false })
            .scroll((viewport.scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_type(BorderType::Plain)
                    .border_style(semantic_style(state, SemanticColor::Muted))
                    .title_style(semantic_style(state, SemanticColor::Muted)),
            ),
        region,
    );
}

pub(super) struct TimelineLines {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) review_logical_start: Option<usize>,
}

pub(super) struct TimelineViewport {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) scroll: u16,
}

pub(super) fn timeline_lines_compact(state: &TuiState, region: Rect) -> TimelineLines {
    let mut lines = Vec::new();
    let mut review_logical_start = None;
    for (index, item) in state.timeline().iter().enumerate() {
        if state.timeline_review_user_index() == Some(index) {
            review_logical_start = Some(lines.len());
        }
        let item_lines = match item {
            TimelineItem::User { text, lane } => user_lines(state, text, *lane),
            TimelineItem::Assistant { text } => assistant_lines(state, text, region.width),
            TimelineItem::Muted { title, detail } => muted_lines(state, title, detail),
            TimelineItem::LocalCommand { title, body } => {
                local_command_lines(state, title, body, region.width)
            }
            TimelineItem::Expanded { title, body } => {
                expanded_timeline_lines(state, title, body, region.width)
            }
            TimelineItem::Diagnostic { title, body } => {
                diagnostic_lines(state, title, body, region.width)
            }
            TimelineItem::Patch { changes } => compact_patch_lines(state, changes),
        };
        let item_lines = item_lines
            .into_iter()
            .flat_map(split_oversized_timeline_line)
            .collect();
        lines.extend(spaced_timeline_item(
            item_lines,
            index + 1 < state.timeline().len(),
        ));
    }
    TimelineLines {
        lines,
        review_logical_start,
    }
}

pub(super) fn timeline_viewport(
    state: &TuiState,
    timeline: TimelineLines,
    region: Rect,
) -> TimelineViewport {
    let mut scroll = timeline_scroll_start(state, &timeline, region);
    let mut lines = timeline.lines;
    let max_scroll = usize::from(u16::MAX);
    if scroll > max_scroll {
        // Paragraph scroll is u16; remove complete prefix lines in one linear pass.
        let rows_to_drop = scroll - max_scroll;
        let mut dropped_lines = 0;
        let mut dropped_rows = 0;
        for line in &lines {
            if dropped_rows >= rows_to_drop {
                break;
            }
            dropped_rows += wrapped_line_count(std::slice::from_ref(line), region.width).max(1);
            dropped_lines += 1;
        }
        if dropped_lines > 0 {
            lines.drain(..dropped_lines);
            scroll = scroll.saturating_sub(dropped_rows);
        }
    }

    TimelineViewport {
        lines,
        scroll: u16::try_from(scroll).unwrap_or(u16::MAX),
    }
}

pub(super) fn timeline_scroll_start(
    state: &TuiState,
    timeline: &TimelineLines,
    region: Rect,
) -> usize {
    if let Some(logical_start) = timeline.review_logical_start {
        wrapped_line_count(&timeline.lines[..logical_start], region.width)
    } else {
        let total = wrapped_line_count(&timeline.lines, region.width);
        let visible = usize::from(region.height.saturating_sub(1));
        total
            .saturating_sub(visible)
            .saturating_sub(state.timeline_scroll_offset())
    }
}

pub(super) fn wrapped_line_count(lines: &[Line<'static>], width: u16) -> usize {
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width)
}

pub(super) fn split_oversized_timeline_line(line: Line<'static>) -> Vec<Line<'static>> {
    let byte_len = line.spans.iter().fold(0_usize, |total, span| {
        total.saturating_add(span.content.len())
    });
    if byte_len <= MAX_TIMELINE_LOGICAL_LINE_GRAPHEMES {
        return vec![line];
    }

    let Line {
        style,
        alignment,
        spans,
    } = line;
    let mut lines = Vec::new();
    let mut current_spans = Vec::new();
    let mut current_graphemes = 0;

    for span in spans {
        let mut chunk = String::new();
        for grapheme in span.styled_graphemes(Style::default()) {
            if current_graphemes == MAX_TIMELINE_LOGICAL_LINE_GRAPHEMES {
                if !chunk.is_empty() {
                    current_spans.push(Span::styled(std::mem::take(&mut chunk), span.style));
                }
                lines.push(Line {
                    style,
                    alignment,
                    spans: std::mem::take(&mut current_spans),
                });
                current_graphemes = 0;
            }
            chunk.push_str(grapheme.symbol);
            current_graphemes += 1;
        }
        if !chunk.is_empty() {
            current_spans.push(Span::styled(chunk, span.style));
        }
    }

    if !current_spans.is_empty() || lines.is_empty() {
        lines.push(Line {
            style,
            alignment,
            spans: current_spans,
        });
    }
    lines
}

pub(super) fn spaced_timeline_item(
    mut lines: Vec<Line<'static>>,
    has_next_item: bool,
) -> Vec<Line<'static>> {
    if has_next_item && !lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

pub(super) fn assistant_lines(
    state: &TuiState,
    text: &str,
    region_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = markdown_lines(state, text, region_width);
    lines.push(assistant_separator_line(state, region_width));
    lines
}

pub(super) fn assistant_separator_line(state: &TuiState, region_width: u16) -> Line<'static> {
    let width = usize::from(region_width).max(1);
    Line::from(Span::styled(
        "-".repeat(width),
        semantic_style(state, SemanticColor::Muted),
    ))
}

pub(super) fn muted_lines(state: &TuiState, title: &str, detail: &str) -> Vec<Line<'static>> {
    if let Some(line) = tool_title_line_from_parts(state, title, detail) {
        return vec![line];
    }

    let mut spans = vec![Span::styled(
        title.to_owned(),
        semantic_style(state, SemanticColor::Muted),
    )];
    if detail.is_empty() {
        return vec![Line::from(spans)];
    }
    spans.push(Span::styled(
        " ",
        semantic_style(state, SemanticColor::Muted),
    ));
    spans.extend(inline_code_spans(
        state,
        detail,
        semantic_style(state, SemanticColor::Muted),
    ));
    vec![Line::from(spans)]
}

pub(super) fn compact_patch_lines(
    state: &TuiState,
    changes: &[PatchChangeView],
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for change in changes {
        lines.push(Line::from(Span::styled(
            format!(
                "Edited {} (+{} -{})",
                change.path, change.added, change.removed
            ),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "  {} hunk(s), {} -> {} bytes",
                change.hunks,
                change
                    .bytes_before
                    .map_or_else(|| "-".to_owned(), |bytes| bytes.to_string()),
                change
                    .bytes_after
                    .map_or_else(|| "-".to_owned(), |bytes| bytes.to_string())
            ),
            semantic_style(state, SemanticColor::Muted),
        )));
    }
    lines
}

pub(super) fn expanded_title_line(state: &TuiState, title: &str) -> Line<'static> {
    if let Some(line) = tool_title_line(state, title) {
        return line;
    }

    Line::from(Span::styled(
        title.to_owned(),
        semantic_style(state, SemanticColor::Focus),
    ))
}

pub(super) fn local_command_lines(
    state: &TuiState,
    title: &str,
    body: &str,
    region_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        title.to_owned(),
        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
    ))];
    let body_width = region_width.saturating_sub(2).max(1);
    lines.extend(
        markdown_lines(state, body, body_width)
            .into_iter()
            .map(|line| {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(line.spans);
                Line::from(spans)
            }),
    );
    lines
}

pub(super) fn expanded_timeline_lines(
    state: &TuiState,
    title: &str,
    body: &str,
    region_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![expanded_title_line(state, title)];
    if tool_title_line(state, title).is_none() {
        return lines;
    }

    let body_width = usize::from(region_width).saturating_sub(2).max(4);
    for line in body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(TOOL_RESULT_PREVIEW_MAX_LINES)
    {
        let clean = line
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        let clean = clean.trim();
        if clean.is_empty() {
            continue;
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", truncate_chars(clean, body_width)),
            semantic_style(state, SemanticColor::Muted),
        )));
    }
    lines
}

pub(super) fn diagnostic_lines(
    state: &TuiState,
    title: &str,
    body: &str,
    region_width: u16,
) -> Vec<Line<'static>> {
    let reason = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("run failed");
    wrap_styled_parts(
        vec![
            StyledTextPart {
                text: "! Error  ".to_owned(),
                style: semantic_style(state, SemanticColor::Error).add_modifier(Modifier::BOLD),
                atomic: true,
            },
            StyledTextPart {
                text: title.to_owned(),
                style: semantic_style(state, SemanticColor::Error).add_modifier(Modifier::BOLD),
                atomic: true,
            },
            StyledTextPart {
                text: format!(": {reason}"),
                style: semantic_style(state, SemanticColor::Assistant),
                atomic: false,
            },
        ],
        region_width,
    )
}

pub(super) fn tool_title_line_from_parts(
    state: &TuiState,
    title: &str,
    detail: &str,
) -> Option<Line<'static>> {
    if detail.is_empty() {
        return tool_title_line(state, title);
    }

    if title == "Ran" {
        return Some(ran_title_line(state, detail));
    }
    tool_title_keyword(title).map(|keyword| tool_keyword_title_line(state, keyword, detail))
}

pub(super) fn tool_title_line(state: &TuiState, title: &str) -> Option<Line<'static>> {
    if let Some(command) = title
        .strip_prefix("Ran ")
        .or_else(|| title.strip_prefix("Ran: "))
    {
        return Some(ran_title_line(state, command));
    }

    for keyword in TOOL_TITLE_KEYWORDS {
        if let Some(detail) = strip_tool_title_detail(title, keyword) {
            return Some(tool_keyword_title_line(state, keyword, detail));
        }
    }

    None
}

pub(super) fn strip_tool_title_detail<'a>(title: &'a str, keyword: &str) -> Option<&'a str> {
    if title == keyword {
        return Some("");
    }
    title.strip_prefix(keyword)?.strip_prefix(' ')
}

pub(super) const TOOL_TITLE_KEYWORDS: &[&str] =
    &["Read", "Searched", "MCP", "Permission", "Patch", "Tool"];

pub(super) fn tool_title_keyword(title: &str) -> Option<&'static str> {
    TOOL_TITLE_KEYWORDS
        .iter()
        .copied()
        .find(|keyword| title == *keyword)
}

pub(super) fn tool_keyword_title_line(
    state: &TuiState,
    keyword: &str,
    detail: &str,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        keyword.to_owned(),
        semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD),
    )];
    if !detail.is_empty() {
        spans.push(Span::styled(
            " ".to_owned(),
            semantic_style(state, SemanticColor::Muted),
        ));
        spans.extend(inline_code_spans(
            state,
            detail,
            semantic_style(state, SemanticColor::Assistant),
        ));
    }
    Line::from(spans)
}

pub(super) fn ran_title_line(state: &TuiState, detail: &str) -> Line<'static> {
    let (command, suffix) = split_command_suffix(detail);
    let mut spans = vec![
        Span::styled(
            "Ran".to_owned(),
            semantic_style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ".to_owned(), semantic_style(state, SemanticColor::Muted)),
    ];
    spans.extend(command_spans(state, command));
    if !suffix.is_empty() {
        spans.push(Span::styled(
            suffix.to_owned(),
            semantic_style(state, SemanticColor::Muted),
        ));
    }
    Line::from(spans)
}

pub(super) fn split_command_suffix(detail: &str) -> (&str, &str) {
    let Some(index) = detail.rfind(" (") else {
        return (detail, "");
    };
    let suffix = &detail[index..];
    let Some(close) = suffix.find(')') else {
        return (detail, "");
    };
    if !suffix[close + 1..].is_empty() && !suffix[close + 1..].starts_with(" -> ") {
        return (detail, "");
    }
    (&detail[..index], suffix)
}

pub(super) fn user_lines(
    state: &TuiState,
    text: &str,
    lane: QueuedInputLane,
) -> Vec<Line<'static>> {
    let lane_label = match lane {
        QueuedInputLane::Next => None,
        QueuedInputLane::Suspended => Some(("suspended", SemanticColor::Warning)),
        QueuedInputLane::Backlog => Some(("backlog", SemanticColor::Muted)),
    };
    let mut lines = Vec::new();
    for (index, segment) in text.split('\n').enumerate() {
        let mut spans = vec![Span::styled(
            "▌ ",
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        )];
        if index == 0
            && let Some((label, color)) = lane_label
        {
            spans.push(Span::styled(
                format!("{label}  "),
                semantic_style(state, color).add_modifier(Modifier::BOLD),
            ));
        }
        spans.extend(inline_code_spans(
            state,
            segment,
            semantic_style(state, SemanticColor::Assistant),
        ));
        lines.push(Line::from(spans));
    }
    lines
}
