//! Timeline content, wrapping, and bounded viewport projection.

use super::activity::{ActivityLayout, paint_activity};
use super::sticky::{paint_sticky_header, split_timeline_content, timeline_scroll_split};
use crate::tui::{
    copy_controls::{CopyContent, CopyTarget, copy_header},
    keymap::KeyAction,
    markdown::{RenderedMarkdown, markdown_lines},
    render::command_style::command_spans,
    state::{
        CommandFailure, CommandView, PatchChangeView, PatchOperationView, TimelineItem,
        ToolOutputPreview, TuiState,
    },
    text_wrap::{
        StyledTextPart, inline_code_spans, semantic_style, truncate_chars, wrap_styled_parts,
        wrap_styled_parts_preserving_leading_whitespace,
    },
    theme::{SemanticColor, dim_color},
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
use unicode_width::UnicodeWidthChar;

pub(super) use super::timeline_layout::prepare_timeline_viewport;
use super::timeline_layout::{
    TimelineLayout, timeline_layout, timeline_scroll_start, timeline_viewport,
};

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
    let timeline = timeline_layout(state, content_region);
    let scroll = timeline_scroll_start(state, &timeline, content_region);
    let split = timeline_scroll_split(
        state,
        &timeline.item_starts,
        &timeline.row_starts,
        &timeline.rows,
        content_region.height,
        scroll,
    );
    let (header, body) = split_timeline_content(content_region, split.header_rows);
    paint_sticky_header(frame, header, &split);
    if body.is_empty() {
        return;
    }
    if let Some(selection) = state.text_selection()
        && selection.area() == body
    {
        selection.render(frame.buffer_mut());
        return;
    }
    let folds = command_fold_marks(state, &timeline, body, split.body_scroll);
    let item_starts = timeline.item_starts.clone();
    let row_starts = timeline.row_starts.clone();
    let blank_lines = blank_logical_lines(&timeline);
    let viewport = timeline_viewport(timeline, split.body_scroll);
    frame.render_widget(
        Paragraph::new(viewport.lines)
            .wrap(Wrap { trim: false })
            .scroll((viewport.scroll, 0)),
        body,
    );
    paint_activity(
        frame,
        state,
        body,
        ActivityLayout {
            item_starts: &item_starts,
            row_starts: &row_starts,
            blank_lines: &blank_lines,
        },
        split.body_scroll,
    );
    paint_command_folds(frame, state, body, &folds);
}

pub(super) fn blank_logical_lines(timeline: &TimelineLayout) -> Vec<bool> {
    timeline
        .rows
        .iter()
        .map(|row| row.display.spans.iter().all(|span| span.content.is_empty()))
        .collect()
}

pub(super) fn assistant_lines(
    state: &TuiState,
    text: &str,
    item_index: usize,
    region_width: u16,
) -> RenderedMarkdown {
    let mut rendered = markdown_lines(state, text, region_width);
    if let Some((header, width)) = copy_header(state, region_width) {
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
            patch_change_title(change),
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            patch_change_detail(change),
            semantic_style(state, SemanticColor::Muted),
        )));
    }
    lines
}

/// Titles a patch change with the verb that matches its file operation.
fn patch_change_title(change: &PatchChangeView) -> String {
    match change.operation {
        PatchOperationView::Add => format!("Created {} (+{})", change.path, change.added),
        PatchOperationView::Update => format!(
            "Edited {} (+{} -{})",
            change.path, change.added, change.removed
        ),
        PatchOperationView::Delete => format!("Deleted {} (-{})", change.path, change.removed),
    }
}

/// Summarizes a patch change by line counts first, then byte counts.
///
/// Reviewers reason about a change in lines, while byte counts describe the
/// file-size effect that a line count cannot express, so lines lead and both
/// stay visible. The hunk count only describes an update's hunks.
fn patch_change_detail(change: &PatchChangeView) -> String {
    let mut segments = Vec::new();
    if change.operation == PatchOperationView::Update {
        segments.push(format!("{} hunk(s)", change.hunks));
    }
    segments.push(format!(
        "{} -> {} lines",
        optional_size(change.lines_before),
        optional_size(change.lines_after)
    ));
    segments.push(format!(
        "{} -> {} bytes",
        change.bytes_before, change.bytes_after
    ));
    format!("  {}", segments.join(", "))
}

/// Renders a stored size that a replayed envelope may not carry.
fn optional_size(value: Option<usize>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
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

    lines.extend(output_preview_lines(
        state,
        &ToolOutputPreview::new(body.lines(), false),
        true,
        region_width,
    ));
    lines
}

pub(super) fn read_output_lines(
    state: &TuiState,
    title: &str,
    preview: &ToolOutputPreview,
    region_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![expanded_title_line(state, title)];
    lines.extend(output_preview_lines(state, preview, false, region_width));
    lines
}

fn output_preview_lines(
    state: &TuiState,
    preview: &ToolOutputPreview,
    always_show: bool,
    region_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !always_show && !state.show_successful_command_output() {
        return lines;
    }
    let body_width = usize::from(region_width).saturating_sub(2).max(4);
    let style = output_panel_style(state);
    for line in &preview.lines {
        let clean = crate::text::without_control_chars(line);
        let clean = clean.trim();
        if clean.is_empty() {
            continue;
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", truncate_chars(clean, body_width)),
            style,
        )));
    }
    if preview.truncated {
        lines.push(Line::from(Span::styled("  ...", style)));
    }
    lines
}

fn output_panel_style(state: &TuiState) -> ratatui::style::Style {
    let mut style = semantic_style(state, SemanticColor::Assistant);
    if let Some(background) = state.theme().color(SemanticColor::CodeBackground) {
        style = style.bg(background);
    }
    style
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
            expanded,
            ..
        } => {
            let failed =
                exit_code.is_some_and(|code| code != 0) || *failure == Some(CommandFailure::Failed);
            let title = finished_command_title(state, detail, *exit_code, *failure, *elapsed);
            // Finished commands close to one line. The gutter chevron opens the
            // command and its output again.
            if !expanded {
                return vec![collapsed_command_title(state, title, region_width)];
            }
            let mut lines = wrap_command_title(title, region_width);
            if failed || failure.is_none() {
                lines.extend(output_preview_lines(state, preview, failed, region_width));
            }
            lines
        }
    }
}

fn finished_command_title(
    state: &TuiState,
    detail: &str,
    exit_code: Option<i64>,
    failure: Option<CommandFailure>,
    elapsed: Option<std::time::Duration>,
) -> Line<'static> {
    let mut title = command_title_line(state, "Ran", detail);
    let failed = exit_code.is_some_and(|code| code != 0) || failure == Some(CommandFailure::Failed);
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
    append_command_elapsed(state, &mut title, elapsed);
    title
}

fn command_title_wraps(title: &Line<'_>, region_width: u16) -> bool {
    line_display_width(title) > usize::from(region_width).max(1)
}

/// `Some(true)` is an open fold, `Some(false)` is a closed one.
fn command_fold(state: &TuiState, view: &CommandView, region_width: u16) -> Option<bool> {
    let CommandView::Finished { expanded, .. } = view else {
        return None;
    };
    finished_command_reveals_body(state, view, region_width).then_some(*expanded)
}

/// Opening the row shows either the rest of a wrapped command or its output.
fn finished_command_reveals_body(state: &TuiState, view: &CommandView, region_width: u16) -> bool {
    let CommandView::Finished {
        detail,
        exit_code,
        failure,
        preview,
        elapsed,
        ..
    } = view
    else {
        return false;
    };
    let title = finished_command_title(state, detail, *exit_code, *failure, *elapsed);
    if command_title_wraps(&title, region_width) {
        return true;
    }
    let failed =
        exit_code.is_some_and(|code| code != 0) || *failure == Some(CommandFailure::Failed);
    let has_preview = preview.truncated || preview.lines.iter().any(|line| !line.trim().is_empty());
    has_preview && (failed || failure.is_none() && state.show_successful_command_output())
}

fn collapsed_command_title(state: &TuiState, title: Line<'static>, width: u16) -> Line<'static> {
    let max_width = usize::from(width);
    if max_width == 0 {
        return Line::from("");
    }
    if line_display_width(&title) <= max_width {
        return title;
    }

    let mut spans = title.spans;
    let prefix_len = spans.len().min(2);
    let mut command = spans.split_off(prefix_len);
    let prefix = spans;
    let prefix_width = span_width(&prefix);

    let mut tail_start = command.len();
    for index in (0..command.len()).rev() {
        let text = command[index].content.as_ref();
        if text.starts_with(" (") || text.starts_with(" ->") || text.starts_with("  ") {
            tail_start = index;
        } else {
            break;
        }
    }
    let mut tail = command.split_off(tail_start);
    let ellipsis_width = 1;
    while !tail.is_empty()
        && prefix_width
            .saturating_add(ellipsis_width)
            .saturating_add(span_width(&tail))
            > max_width
    {
        tail.pop();
    }

    let command_budget = max_width
        .saturating_sub(prefix_width)
        .saturating_sub(ellipsis_width)
        .saturating_sub(span_width(&tail));
    let mut fitted = prefix;
    fitted.extend(truncate_spans(&command, command_budget));
    fitted.push(Span::styled(
        "…".to_owned(),
        semantic_style(state, SemanticColor::Muted),
    ));
    fitted.extend(tail);
    let line = Line::from(fitted);
    if line_display_width(&line) <= max_width {
        line
    } else {
        Line::from(truncate_spans(&line.spans, max_width))
    }
}

fn line_display_width(line: &Line<'_>) -> usize {
    span_width(&line.spans)
}

fn span_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

fn truncate_spans(spans: &[Span<'static>], max_width: usize) -> Vec<Span<'static>> {
    let mut kept = Vec::new();
    let mut used = 0_usize;
    for span in spans {
        let width = span.width();
        if used.saturating_add(width) <= max_width {
            kept.push(span.clone());
            used = used.saturating_add(width);
            continue;
        }
        let room = max_width.saturating_sub(used);
        let text = truncate_to_width(span.content.as_ref(), room);
        if !text.is_empty() {
            kept.push(Span::styled(text, span.style));
        }
        break;
    }
    kept
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut width = 0_usize;
    let mut end = 0_usize;
    for (index, character) in text.char_indices() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width.saturating_add(character_width) > max_width {
            break;
        }
        width = width.saturating_add(character_width);
        end = index.saturating_add(character.len_utf8());
    }
    text[..end].to_owned()
}

struct CommandFoldMark {
    local_y: u16,
    open: bool,
}

fn command_fold_marks(
    state: &TuiState,
    timeline: &TimelineLayout,
    content: Rect,
    scroll: usize,
) -> Vec<CommandFoldMark> {
    if content.x == 0 || content.height == 0 {
        return Vec::new();
    }
    let mut marks = Vec::new();
    for (index, item) in state.timeline().iter().enumerate() {
        let TimelineItem::Command { view } = item else {
            continue;
        };
        let Some(open) = command_fold(state, view, content.width) else {
            continue;
        };
        let Some(&logical) = timeline.item_starts.get(index) else {
            continue;
        };
        let Some(&visual) = timeline.row_starts.get(logical) else {
            continue;
        };
        if visual < scroll {
            continue;
        }
        let local = visual - scroll;
        if local >= usize::from(content.height) {
            continue;
        }
        let Ok(local_y) = u16::try_from(local) else {
            continue;
        };
        marks.push(CommandFoldMark { local_y, open });
    }
    marks
}

fn paint_command_folds(
    frame: &mut Frame<'_>,
    state: &TuiState,
    content: Rect,
    folds: &[CommandFoldMark],
) {
    if content.x == 0 {
        return;
    }
    let style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    for fold in folds {
        let y = content.y.saturating_add(fold.local_y);
        let cell = &mut frame.buffer_mut()[(content.x - 1, y)];
        cell.set_symbol(if fold.open { "▾" } else { "▸" });
        cell.set_style(style);
    }
}

pub(super) fn command_fold_hit(
    state: &TuiState,
    content: Rect,
    timeline: &TimelineLayout,
    scroll: usize,
    position: Position,
) -> Option<usize> {
    if content.height == 0 || position.y < content.y || position.y >= content.bottom() {
        return None;
    }
    let in_gutter = content.x > 0 && position.x + 1 == content.x;
    let in_content = content.contains(position);
    if !in_gutter && !in_content {
        return None;
    }
    let visual = scroll.saturating_add(usize::from(position.y - content.y));
    let logical = logical_line_at_visual(&timeline.row_starts, visual)?;
    let index = item_index_at_logical(&timeline.item_starts, logical)?;
    if timeline.item_starts.get(index).copied() != Some(logical) {
        return None;
    }
    let TimelineItem::Command { view } = state.timeline().get(index)? else {
        return None;
    };
    let open = command_fold(state, view, content.width)?;
    let first_visual = timeline.row_starts.get(logical).copied()?;
    if visual != first_visual || (open && !in_gutter) {
        return None;
    }
    Some(index)
}

fn logical_line_at_visual(row_starts: &[usize], visual: usize) -> Option<usize> {
    let end = *row_starts.last()?;
    if visual >= end {
        return None;
    }
    Some(row_starts.partition_point(|start| *start <= visual) - 1)
}

fn item_index_at_logical(item_starts: &[usize], logical: usize) -> Option<usize> {
    if item_starts.is_empty() || logical < item_starts[0] {
        return None;
    }
    Some(item_starts.partition_point(|start| *start <= logical) - 1)
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

const USER_PROMPT_MARK: &str = "\u{276F} ";
const USER_PROMPT_INDENT: &str = "  ";

pub(super) fn user_lines(
    state: &TuiState,
    text: &str,
    lane: QueuedInputLane,
    width: u16,
) -> Vec<Line<'static>> {
    let lane_label = match lane {
        QueuedInputLane::Next => None,
        QueuedInputLane::Suspended => Some(("suspended", SemanticColor::Warning)),
        QueuedInputLane::Backlog => Some(("backlog", SemanticColor::Muted)),
    };
    let band = state.theme().color(SemanticColor::Focus).map(dim_color);
    let mark_style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    let body_style = semantic_style(state, SemanticColor::Assistant).add_modifier(Modifier::BOLD);
    let segments: Vec<&str> = if text.is_empty() {
        vec![""]
    } else {
        text.split('\n').collect()
    };
    segments
        .into_iter()
        .enumerate()
        .map(|(index, segment)| {
            let mut spans = vec![Span::styled(
                if index == 0 {
                    USER_PROMPT_MARK
                } else {
                    USER_PROMPT_INDENT
                }
                .to_owned(),
                mark_style,
            )];
            if index == 0
                && let Some((label, color)) = lane_label
            {
                spans.push(Span::styled(
                    format!("{label}  "),
                    semantic_style(state, color).add_modifier(Modifier::BOLD),
                ));
            }
            spans.extend(inline_code_spans(state, segment, body_style));
            let mut line = Line::from(spans);
            if let Some(color) = band {
                line.style.bg = Some(color);
            }
            pad_user_line(&mut line, width);
            line
        })
        .collect()
}

fn pad_user_line(line: &mut Line<'static>, width: u16) {
    let columns = usize::from(width);
    let current = line.width();
    if columns > current {
        line.push_span(" ".repeat(columns - current));
    }
}
