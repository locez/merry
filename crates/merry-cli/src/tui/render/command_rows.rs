//! Command row formatting, horizontal running shimmer, output previews, and fold gutters.

use super::timeline::{expanded_title_line, tool_title_line};
use super::timeline_cache::TimelineLayoutSnapshot;
use crate::tui::{
    motion::{self, MotionClock},
    render::command_style::command_spans,
    state::{CommandFailure, CommandView, TimelineItem, ToolOutputPreview, TuiState},
    text_wrap::{
        StyledTextPart, inline_code_spans, semantic_style, truncate_chars,
        wrap_styled_parts_preserving_leading_whitespace,
    },
    theme::SemanticColor,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Modifier,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

pub(super) fn output_preview_lines(
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

/// Renders a live rolling tail for an expanded running command, mirroring
/// Thinking's tail behavior (`  …` at the top when older lines scroll off,
/// newest output always visible at the bottom).
fn live_output_lines(
    state: &TuiState,
    live_preview: &crate::tui::state::LiveCommandOutput,
    region_width: u16,
) -> Vec<Line<'static>> {
    let (tail_lines, older_omitted) = live_preview.latest_tail();
    let body_width = usize::from(region_width).saturating_sub(2).max(4);
    let style = output_panel_style(state);
    let mut lines = Vec::new();
    if older_omitted {
        lines.push(Line::from(Span::styled(
            "  …".to_owned(),
            semantic_style(state, SemanticColor::Muted),
        )));
    }
    for line in tail_lines {
        let clean = crate::text::without_control_chars(&line);
        let clean = clean.trim();
        if clean.is_empty() {
            continue;
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", truncate_chars(clean, body_width)),
            style,
        )));
    }
    lines
}

fn output_panel_style(state: &TuiState) -> ratatui::style::Style {
    // Keep command output transparent so expanded previews blend naturally on
    // translucent terminals instead of painting uneven solid rectangles.
    semantic_style(state, SemanticColor::TextSecondary)
}

pub(super) fn command_lines(
    state: &TuiState,
    view: &CommandView,
    region_width: u16,
) -> Vec<Line<'static>> {
    match view {
        CommandView::Running {
            detail,
            started_at,
            live_preview,
            expanded,
        } => {
            let elapsed = started_at.elapsed();
            let title = running_command_title(state, detail, elapsed);
            let mut lines = render_running_tool_lines(state, title, *expanded, region_width);
            if *expanded && live_preview.has_output() {
                lines.extend(live_output_lines(state, live_preview, region_width));
            }
            lines
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

/// Renders an active non-process tool call (`read_file`, `apply_patch`, `MCP`,
/// etc.) using the exact same spinner, soft lavender-pink flow shimmer, and
/// single-line collapse behavior as running shell commands.
pub(super) fn tool_running_lines(
    state: &TuiState,
    title: &str,
    detail: &str,
    started_at: tokio::time::Instant,
    expanded: bool,
    region_width: u16,
) -> Vec<Line<'static>> {
    let elapsed = started_at.elapsed();
    let line = running_generic_tool_title(state, title, detail, elapsed);
    render_running_tool_lines(state, line, expanded, region_width)
}

/// Renders a finished `Read` tool row with the same single-line collapse and
/// click-to-expand preview behavior as finished commands.
pub(super) fn read_tool_lines(
    state: &TuiState,
    title: &str,
    preview: &ToolOutputPreview,
    expanded: bool,
    region_width: u16,
) -> Vec<Line<'static>> {
    let title_line = expanded_title_line(state, title);
    if !expanded {
        return vec![collapsed_command_title(state, title_line, region_width)];
    }
    let mut lines = wrap_command_title(title_line, region_width);
    lines.extend(output_preview_lines(state, preview, false, region_width));
    lines
}

/// Renders a finished generic/MCP/permission tool row with the same
/// single-line collapse and click-to-expand preview behavior as finished
/// commands.
pub(super) fn expanded_tool_lines(
    state: &TuiState,
    title: &str,
    body: &str,
    expanded: bool,
    region_width: u16,
) -> Vec<Line<'static>> {
    let title_line = expanded_title_line(state, title);
    if !expanded {
        return vec![collapsed_command_title(state, title_line, region_width)];
    }
    let mut lines = wrap_command_title(title_line, region_width);
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

fn render_running_tool_lines(
    state: &TuiState,
    title: Line<'static>,
    expanded: bool,
    region_width: u16,
) -> Vec<Line<'static>> {
    let clock = MotionClock::now();
    let animated = state.animations_enabled();
    let title_lines = if expanded {
        wrap_command_title(title, region_width)
    } else {
        vec![collapsed_command_title(state, title, region_width)]
    };
    motion::flow_gradient_lines(
        title_lines,
        motion::RUNNING_FLOW_STOPS,
        motion::RUNNING_FLOW_WAVELENGTH,
        motion::RUNNING_FLOW_CHARS_PER_SEC,
        state.theme().color(SemanticColor::Muted),
        clock,
        animated,
    )
}

fn running_generic_tool_title(
    state: &TuiState,
    title: &str,
    detail: &str,
    elapsed: std::time::Duration,
) -> Line<'static> {
    let spinner = motion::spinner_glyph(elapsed);
    let mut spans = vec![Span::styled(
        format!("{spinner} {title}"),
        semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
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
    let mut line = Line::from(spans);
    append_command_elapsed(state, &mut line, Some(elapsed));
    line
}

fn running_command_title(
    state: &TuiState,
    detail: &str,
    elapsed: std::time::Duration,
) -> Line<'static> {
    let spinner = motion::spinner_glyph(elapsed);
    let mut title = command_title_line_with_color(
        state,
        &format!("{spinner} Running"),
        SemanticColor::Focus,
        detail,
    );
    append_command_elapsed(state, &mut title, Some(elapsed));
    title
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
/// Unified across commands, active tools, `Read`, and generic tool outputs.
fn tool_fold(state: &TuiState, item: &TimelineItem, region_width: u16) -> Option<bool> {
    match item {
        TimelineItem::Command { view } => command_fold(state, view, region_width),
        TimelineItem::ToolRunning {
            title,
            detail,
            started_at,
            expanded,
        } => {
            let line = running_generic_tool_title(state, title, detail, started_at.elapsed());
            command_title_wraps(&line, region_width).then_some(*expanded)
        }
        TimelineItem::Read {
            title,
            preview,
            expanded,
        } => {
            let wraps = command_title_wraps(&expanded_title_line(state, title), region_width);
            let has_preview = state.show_successful_command_output()
                && (preview.truncated || preview.lines.iter().any(|line| !line.trim().is_empty()));
            (wraps || has_preview).then_some(*expanded)
        }
        TimelineItem::Expanded {
            title,
            body,
            expanded,
        } => {
            let wraps = command_title_wraps(&expanded_title_line(state, title), region_width);
            let has_preview = tool_title_line(state, title).is_some()
                && body.lines().any(|line| !line.trim().is_empty());
            (wraps || has_preview).then_some(*expanded)
        }
        TimelineItem::Patch { changes, expanded } => (!changes.is_empty()).then_some(*expanded),
        _ => None,
    }
}

/// `Some(true)` is an open fold, `Some(false)` is a closed one.
fn command_fold(state: &TuiState, view: &CommandView, region_width: u16) -> Option<bool> {
    match view {
        CommandView::Running {
            detail,
            started_at,
            live_preview,
            expanded,
        } => {
            let wraps = command_title_wraps(
                &running_command_title(state, detail, started_at.elapsed()),
                region_width,
            );
            (live_preview.has_output() || wraps).then_some(*expanded)
        }
        CommandView::Finished { expanded, .. } => {
            finished_command_reveals_body(state, view, region_width).then_some(*expanded)
        }
    }
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

pub(super) struct CommandFoldMark {
    local_y: u16,
    open: bool,
}

pub(super) fn command_fold_marks(
    state: &TuiState,
    timeline: &TimelineLayoutSnapshot,
    content: Rect,
    scroll: usize,
) -> Vec<CommandFoldMark> {
    if content.x == 0 || content.height == 0 {
        return Vec::new();
    }
    let mut marks = Vec::new();
    let range_end = scroll.saturating_add(usize::from(content.height));
    let first_item = timeline
        .item_visual_starts
        .partition_point(|start| *start <= scroll)
        .saturating_sub(1);
    for (index, item) in state.timeline().iter().enumerate().skip(first_item) {
        let Some(&visual) = timeline.item_visual_starts.get(index) else {
            break;
        };
        if visual >= range_end {
            break;
        }
        if visual < scroll {
            continue;
        }
        let Some(open) = tool_fold(state, item, content.width) else {
            continue;
        };
        let Ok(local_y) = u16::try_from(visual - scroll) else {
            continue;
        };
        marks.push(CommandFoldMark { local_y, open });
    }
    marks
}

pub(super) fn paint_command_folds(
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
    timeline: &TimelineLayoutSnapshot,
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
    let index = timeline.item_at_visual(visual)?;
    let item = state.timeline().get(index)?;
    let open = tool_fold(state, item, content.width)?;
    let first_visual = timeline.item_visual_starts.get(index).copied()?;
    if visual != first_visual || (open && !in_gutter) {
        return None;
    }
    Some(index)
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

pub(super) fn command_title_line(state: &TuiState, keyword: &str, detail: &str) -> Line<'static> {
    command_title_line_with_color(state, keyword, SemanticColor::ToolKeyword, detail)
}

fn command_title_line_with_color(
    state: &TuiState,
    keyword: &str,
    keyword_color: SemanticColor,
    detail: &str,
) -> Line<'static> {
    let (command, suffix) = split_command_suffix(detail);
    let mut spans = vec![
        Span::styled(
            keyword.to_owned(),
            semantic_style(state, keyword_color).add_modifier(Modifier::BOLD),
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
