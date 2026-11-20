//! Builds the cached transcript layout shared by drawing, hit testing, and selection.

use super::{
    activity::thinking_lines,
    timeline::{
        assistant_lines, command_lines, diagnostic_lines, expanded_tool_lines, local_command_lines,
        muted_lines, patch_lines, read_tool_lines, subagent_lines, tool_running_lines, user_lines,
    },
    timeline_cache::{
        CachedItemLayout, NextItemKind, TimelineCacheContext, TimelineLayoutSnapshot,
    },
};
use crate::tui::{
    state::{CommandView, TimelineItem, TuiState},
    transcript::{SelectionPolicy, TranscriptRow},
};
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Wrap},
};

// Keep prefix eviction below the viewport start when Paragraph scroll exceeds u16.
pub(super) const MAX_TIMELINE_LOGICAL_LINE_GRAPHEMES: usize = 32_768;

pub(super) fn timeline_layout(state: &TuiState, region: Rect) -> TimelineLayoutSnapshot {
    let timeline = state.timeline();
    let animations = state.animations_enabled();
    let context = TimelineCacheContext {
        width: region.width,
        max_reading_width: state.max_reading_width(),
        animations,
        code_theme: state.code_theme(),
        show_successful_command_output: state.show_successful_command_output(),
        theme: state.theme().clone(),
    };
    state.timeline_cache().resolve_snapshot(
        context,
        timeline.len(),
        |index| {
            let next_kind = match timeline.get(index + 1) {
                None => NextItemKind::None,
                Some(next) if is_compact_activity_item(next) => NextItemKind::Command,
                Some(_) => NextItemKind::Other,
            };
            let volatile = matches!(
                timeline.get(index),
                Some(
                    TimelineItem::Command {
                        view: CommandView::Running { .. }
                    } | TimelineItem::ToolRunning { .. }
                )
            ) || (animations
                && matches!(
                    timeline.get(index),
                    Some(TimelineItem::Thinking { elapsed: None, .. })
                ))
                || matches!(
                    timeline.get(index),
                    Some(TimelineItem::Subagent { agent_id, .. })
                        if state
                            .subagent_by_id(agent_id)
                            .is_some_and(|entry| entry.phase.is_active())
                );
            (state.timeline_item_version(index), next_kind, volatile)
        },
        |index| compute_item_layout(state, index, region.width),
    )
}

fn compute_item_layout(state: &TuiState, index: usize, width: u16) -> CachedItemLayout {
    let timeline = state.timeline();
    let item = &timeline[index];
    let mut item_targets = Vec::new();
    let item_rows = match item {
        TimelineItem::User { text, lane } => user_lines(state, text, *lane, width),
        TimelineItem::Assistant { text } => {
            let rendered = assistant_lines(state, text, index, width);
            item_targets = rendered.copy_targets;
            rendered.rows
        }
        TimelineItem::Thinking {
            text,
            started_at,
            elapsed,
            expanded,
        } => thinking_lines(state, text, *started_at, *elapsed, *expanded, width)
            .into_iter()
            .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
            .collect(),
        TimelineItem::Muted { title, detail } => muted_lines(state, title, detail)
            .into_iter()
            .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
            .collect(),
        TimelineItem::Command { view } => command_lines(state, view, width)
            .into_iter()
            .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
            .collect(),
        TimelineItem::ToolRunning {
            title,
            detail,
            started_at,
            expanded,
        } => tool_running_lines(state, title, detail, *started_at, *expanded, width)
            .into_iter()
            .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
            .collect(),
        TimelineItem::Read {
            title,
            preview,
            expanded,
        } => read_tool_lines(state, title, preview, *expanded, width)
            .into_iter()
            .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
            .collect(),
        TimelineItem::LocalCommand { title, body } => {
            let rendered = local_command_lines(state, title, body, width);
            item_targets = rendered.copy_targets;
            rendered.rows
        }
        TimelineItem::Expanded {
            title,
            body,
            expanded,
        } => expanded_tool_lines(state, title, body, *expanded, width)
            .into_iter()
            .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
            .collect(),
        TimelineItem::Diagnostic { title, body } => diagnostic_lines(state, title, body, width)
            .into_iter()
            .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
            .collect(),
        TimelineItem::Patch { changes, expanded } => patch_lines(state, changes, *expanded, width)
            .into_iter()
            .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
            .collect(),
        TimelineItem::Subagent { agent_id, task_id } => {
            subagent_lines(state, agent_id, task_id, width)
                .into_iter()
                .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
                .collect()
        }
    };

    let compact_activity = item_rows.len() == 1
        && is_compact_activity_item(item)
        && timeline
            .get(index + 1)
            .is_some_and(is_compact_activity_item);
    let item_rows =
        spaced_timeline_item(item_rows, index + 1 < timeline.len() && !compact_activity);

    let mut rows = Vec::with_capacity(item_rows.len());
    let mut copy_targets = Vec::with_capacity(item_targets.len());
    let mut item_targets = item_targets.into_iter().peekable();
    for (line_index, row) in item_rows.into_iter().enumerate() {
        while item_targets
            .peek()
            .is_some_and(|target| target.line_index == line_index)
        {
            if let Some(mut target) = item_targets.next() {
                target.line_index = rows.len();
                copy_targets.push(target);
            }
        }
        rows.extend(
            split_oversized_timeline_line(row.display)
                .into_iter()
                .map(|display| TranscriptRow::new(display, row.selection)),
        );
    }

    let mut local_row_starts: Vec<usize> = Vec::with_capacity(rows.len().saturating_add(1));
    local_row_starts.push(0);
    for row in &rows {
        let count = wrapped_line_count(std::slice::from_ref(&row.display), width).max(1);
        let next = local_row_starts
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(count);
        local_row_starts.push(next);
    }

    CachedItemLayout::new(rows, local_row_starts, copy_targets)
}

pub(super) fn timeline_scroll_start(
    state: &TuiState,
    timeline: &TimelineLayoutSnapshot,
    region: Rect,
) -> usize {
    if let Some(anchor) = state.timeline_anchor()
        && let Some(&start) = timeline.item_visual_starts.get(anchor.item_index)
        && let Some(item) = timeline.items.get(anchor.item_index)
    {
        let item_rows = item.visual_height();
        start.saturating_add(anchor.row.min(item_rows.saturating_sub(1)))
    } else if let Some(review_index) = state.timeline_review_user_index()
        && let Some(&start) = timeline.item_visual_starts.get(review_index)
    {
        start
    } else {
        let total = timeline.total_rows();
        let visible = usize::from(region.height);
        total
            .saturating_sub(visible)
            .saturating_sub(state.timeline_scroll_offset())
    }
}

pub(super) fn prepare_timeline_viewport(state: &mut TuiState, region: Rect) {
    let content = timeline_content_region(region);
    if !state.is_timeline_detached() || content.is_empty() {
        return;
    }
    let timeline = timeline_layout(state, content);
    let scroll = timeline_scroll_start(state, &timeline, content);
    let total = timeline.total_rows();
    if let Some(anchor) = timeline.anchor_at(scroll) {
        state.record_timeline_viewport(
            anchor,
            total
                .saturating_sub(usize::from(content.height))
                .saturating_sub(scroll),
        );
    }
}

pub(super) fn timeline_padding(region: Rect) -> Padding {
    let top = u16::from(region.height >= 6);
    let bottom = u16::from(region.height >= 12);
    if region.width >= 80 {
        Padding::new(2, 2, top, bottom)
    } else if region.width >= 50 {
        Padding::new(1, 1, top, bottom)
    } else {
        Padding::new(1, 0, top, bottom)
    }
}

pub(crate) fn timeline_content_region(region: Rect) -> Rect {
    Block::default()
        .borders(Borders::NONE)
        .padding(timeline_padding(region))
        .inner(region)
}

fn is_compact_activity_item(item: &TimelineItem) -> bool {
    matches!(
        item,
        TimelineItem::Command { .. }
            | TimelineItem::ToolRunning { .. }
            | TimelineItem::Read { .. }
            | TimelineItem::Expanded { .. }
            | TimelineItem::Patch {
                expanded: false,
                ..
            }
            | TimelineItem::Muted { .. }
            | TimelineItem::Thinking {
                elapsed: Some(_),
                expanded: false,
                ..
            }
    )
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
    mut rows: Vec<TranscriptRow>,
    has_next_item: bool,
) -> Vec<TranscriptRow> {
    if has_next_item && !rows.is_empty() {
        rows.push(TranscriptRow::new(Line::from(""), SelectionPolicy::Keep));
    }
    rows
}
