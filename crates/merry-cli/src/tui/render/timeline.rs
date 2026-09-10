//! Timeline content, wrapping, and bounded viewport projection.

use crate::tui::{
    copy_controls::{CopyContent, CopyTarget, CopyTextError, copy_header, validate_copy_text},
    keymap::KeyAction,
    markdown::{RenderedMarkdown, markdown_lines},
    render::command_style::command_spans,
    state::{CommandFailure, CommandView, PatchChangeView, TimelineItem, TuiState},
    text_interaction::TextSelection,
    text_wrap::{
        StyledTextPart, inline_code_spans, semantic_style, truncate_chars, wrap_styled_parts,
        wrap_styled_parts_preserving_leading_whitespace,
    },
    theme::SemanticColor,
    transcript::{SelectionPolicy, TranscriptRow},
};
use merry_core::QueuedInputLane;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap},
};

pub(super) use super::timeline_layout::prepare_timeline_viewport;
pub(crate) use super::timeline_layout::timeline_content_region;
use super::timeline_layout::{
    TimelineLayout, timeline_layout, timeline_scroll_start, timeline_viewport,
};

// Keep prefix eviction below the viewport start when Paragraph scroll exceeds u16.
pub(super) const TOOL_RESULT_PREVIEW_MAX_LINES: usize = 5;

pub(super) fn render_timeline_pane(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    let mut block = Block::default()
        .borders(Borders::BOTTOM)
        .border_type(BorderType::Plain)
        .border_style(semantic_style(state, SemanticColor::Muted))
        .title_style(semantic_style(state, SemanticColor::Muted))
        .padding(Padding::left(1));
    let mut review_hint_width = 0;
    if state.is_timeline_detached() {
        let key = state.keymap().binding_label_for(KeyAction::FollowLatest);
        let label = if state.timeline_has_updates() {
            "New content"
        } else {
            "Reviewing"
        };
        let hint = key
            .map(|key| format!(" · {key} latest"))
            .unwrap_or_default();
        let title = Line::from(format!(" {label}{hint} "));
        review_hint_width = title.width();
        block = block.title_bottom(title);
    }
    if state.overlay().is_none()
        && let Some(key) = state
            .keymap()
            .binding_label_for(KeyAction::OpenCommandDetails)
        && state.timeline().iter().rev().any(|item| {
            matches!(
                item,
                TimelineItem::Command {
                    view: CommandView::Finished { .. }
                }
            )
        })
    {
        let hint = Line::from(format!(" {key} output ")).right_aligned();
        if review_hint_width.saturating_add(hint.width()) <= usize::from(region.width) {
            block = block.title_bottom(hint);
        }
    }
    let content_region = block.inner(region);
    frame.render_widget(block, region);
    if content_region.is_empty() {
        return;
    }
    if let Some(selection) = state.text_selection()
        && selection.area() == content_region
    {
        selection.render(frame.buffer_mut());
        return;
    }
    let timeline = timeline_layout(state, content_region);
    let viewport = timeline_viewport(state, timeline, content_region);
    frame.render_widget(
        Paragraph::new(viewport.lines)
            .wrap(Wrap { trim: false })
            .scroll((viewport.scroll, 0)),
        content_region,
    );
}

pub(super) fn assistant_lines(
    state: &TuiState,
    text: &str,
    item_index: usize,
    region_width: u16,
) -> RenderedMarkdown {
    let mut rendered = markdown_lines(state, text, region_width);
    if let Some((header, width)) = copy_header(state, "[Copy reply]", region_width) {
        rendered
            .rows
            .insert(0, TranscriptRow::new(header, SelectionPolicy::Skip));
        for target in &mut rendered.copy_targets {
            target.line_index += 1;
        }
        rendered.copy_targets.insert(
            0,
            CopyTarget::new(0, width, CopyContent::AssistantMessage(item_index)),
        );
    }
    rendered.rows.push(TranscriptRow::new(
        assistant_separator_line(state, region_width),
        SelectionPolicy::Skip,
    ));
    rendered
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
) -> RenderedMarkdown {
    let mut rows = vec![TranscriptRow::new(
        Line::from(Span::styled(
            title.to_owned(),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        )),
        SelectionPolicy::Skip,
    )];
    let body_width = region_width.saturating_sub(1).max(1);
    let mut rendered = markdown_lines(state, body, body_width);
    rows.extend(rendered.rows.into_iter().map(|row| {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(row.display.spans);
        TranscriptRow::new(Line::from(spans), row.selection.with_prefix(1))
    }));
    rendered.rows = rows;
    for target in &mut rendered.copy_targets {
        target.line_index += 1;
        target.column += 1;
    }
    rendered
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TimelineMouseDown {
    None,
    Copy(String),
    CopyTooLarge,
    Select(TextSelection),
}

/// Resolves a timeline press from one layout snapshot.
pub(crate) fn timeline_mouse_down(
    state: &TuiState,
    region: Rect,
    position: Position,
) -> TimelineMouseDown {
    let content = timeline_content_region(region);
    if !region.contains(position) || content.is_empty() {
        return TimelineMouseDown::None;
    }
    let timeline = timeline_layout(state, content);
    if content.contains(position) {
        match copy_target_text(state, &timeline, content, position) {
            Ok(Some(text)) => return TimelineMouseDown::Copy(text),
            Ok(None) => {}
            Err(CopyTextError::TooLarge) => return TimelineMouseDown::CopyTooLarge,
        }
    }
    let position = Position::new(
        position
            .x
            .clamp(content.x, content.right().saturating_sub(1)),
        position
            .y
            .clamp(content.y, content.bottom().saturating_sub(1)),
    );
    let scroll = timeline_scroll_start(state, &timeline, content);
    match TextSelection::new(
        timeline.rows,
        timeline.item_starts,
        timeline.row_starts,
        content,
        scroll,
        position,
    ) {
        Some(selection) => TimelineMouseDown::Select(selection),
        None => TimelineMouseDown::None,
    }
}

fn copy_target_text(
    state: &TuiState,
    timeline: &TimelineLayout,
    content: Rect,
    position: Position,
) -> Result<Option<String>, CopyTextError> {
    let clicked_row = timeline_scroll_start(state, timeline, content)
        .saturating_add(usize::from(position.y - content.y));
    let column = position.x - content.x;
    for target in &timeline.copy_targets {
        let Some(&target_row) = timeline.row_starts.get(target.line_index) else {
            continue;
        };
        if target_row > clicked_row {
            break;
        }
        if target_row == clicked_row
            && (target.column..target.column.saturating_add(target.width)).contains(&column)
        {
            return match &target.content {
                CopyContent::Text(text) => validate_copy_text(text.clone()).map(Some),
                CopyContent::AssistantMessage(index) => match state.timeline().get(*index) {
                    Some(TimelineItem::Assistant { text }) => {
                        if text.len() > crate::tui::copy_controls::MAX_CLIPBOARD_BYTES {
                            Err(CopyTextError::TooLarge)
                        } else {
                            Ok(Some(text.clone()))
                        }
                    }
                    _ => Ok(None),
                },
            };
        }
    }
    Ok(None)
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

pub(super) fn command_lines(
    state: &TuiState,
    view: &CommandView,
    region_width: u16,
) -> Vec<Line<'static>> {
    match view {
        CommandView::Running { detail, started_at } => {
            let elapsed = started_at.elapsed();
            let spinner = match (elapsed.as_millis() / 100) % 8 {
                0 => "⠋",
                1 => "⠙",
                2 => "⠹",
                3 => "⠸",
                4 => "⠼",
                5 => "⠴",
                6 => "⠦",
                _ => "⠧",
            };
            let mut title = command_title_line(state, &format!("Running {spinner}"), detail);
            append_command_elapsed(state, &mut title, Some(elapsed));
            wrap_command_title(title, region_width)
        }
        CommandView::Finished {
            detail,
            exit_code,
            failure,
            preview,
            elapsed,
            ..
        } => {
            let mut title = command_title_line(state, "Ran", detail);
            let failed =
                exit_code.is_some_and(|code| code != 0) || *failure == Some(CommandFailure::Failed);
            if let Some(failure) = failure {
                let (label, color) = match failure {
                    CommandFailure::Cancelled => ("cancelled", SemanticColor::Warning),
                    CommandFailure::Failed => ("failed", SemanticColor::Error),
                };
                title.spans.push(Span::styled(
                    format!(" -> {label}"),
                    semantic_style(state, color).add_modifier(Modifier::BOLD),
                ));
            } else if let Some(code) = exit_code.filter(|_| failed) {
                title.spans.push(Span::styled(
                    format!(" -> {code}"),
                    semantic_style(state, SemanticColor::Error).add_modifier(Modifier::BOLD),
                ));
            }
            append_command_elapsed(state, &mut title, *elapsed);
            let mut lines = wrap_command_title(title, region_width);
            if failed || (failure.is_none() && state.show_successful_command_output()) {
                let body_width = usize::from(region_width).saturating_sub(2).max(4);
                for line in &preview.lines {
                    let clean = line
                        .chars()
                        .filter(|character| !character.is_control())
                        .collect::<String>();
                    lines.push(Line::from(Span::styled(
                        format!("  {}", truncate_chars(clean.trim(), body_width)),
                        semantic_style(state, SemanticColor::Muted),
                    )));
                }
                if preview.truncated {
                    lines.push(Line::from(Span::styled(
                        "  ...",
                        semantic_style(state, SemanticColor::Muted),
                    )));
                }
            }
            lines
        }
    }
}

fn append_command_elapsed(
    state: &TuiState,
    line: &mut Line<'static>,
    elapsed: Option<std::time::Duration>,
) {
    if let Some(elapsed) = elapsed.filter(|elapsed| elapsed.as_secs() >= 1) {
        line.spans.push(Span::styled(
            format!("  {:.1}s", elapsed.as_secs_f64()),
            semantic_style(state, SemanticColor::Muted),
        ));
    }
}

fn wrap_command_title(title: Line<'static>, width: u16) -> Vec<Line<'static>> {
    let prefix_width = title.spans.iter().take(2).map(Span::width).sum::<usize>();
    let indent = if usize::from(width) > prefix_width {
        prefix_width
    } else {
        0
    };
    let mut spans = title.spans;
    let prefix = if indent > 0 {
        spans.drain(..2).collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let parts = spans
        .into_iter()
        .map(|span| StyledTextPart {
            text: span.content.into_owned(),
            style: span.style,
            atomic: false,
        })
        .collect();
    let body_width = width
        .saturating_sub(u16::try_from(indent).unwrap_or_default())
        .max(1);
    wrap_styled_parts_preserving_leading_whitespace(parts, body_width)
        .into_iter()
        .enumerate()
        .map(|(index, mut line)| {
            let mut leading = if index == 0 {
                prefix.clone()
            } else {
                vec![Span::raw(" ".repeat(indent))]
            };
            leading.append(&mut line.spans);
            Line::from(leading)
        })
        .collect()
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
        return Some(command_title_line(state, "Ran", detail));
    }
    tool_title_keyword(title).map(|keyword| tool_keyword_title_line(state, keyword, detail))
}

pub(super) fn tool_title_line(state: &TuiState, title: &str) -> Option<Line<'static>> {
    if let Some(command) = title
        .strip_prefix("Ran ")
        .or_else(|| title.strip_prefix("Ran: "))
    {
        return Some(command_title_line(state, "Ran", command));
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

fn command_title_line(state: &TuiState, keyword: &str, detail: &str) -> Line<'static> {
    let (command, suffix) = split_command_suffix(detail);
    let mut spans = vec![
        Span::styled(
            keyword.to_owned(),
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
