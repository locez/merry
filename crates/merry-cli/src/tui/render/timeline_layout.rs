//! Builds the cached transcript layout shared by drawing, hit testing, and selection.

use super::{
    activity::thinking_lines,
    sticky::pinned_user_index_for_scroll,
    timeline::{
        assistant_lines, command_lines, diagnostic_lines, expanded_tool_lines, local_command_lines,
        muted_lines, patch_lines, read_tool_lines, subagent_lines, tool_running_lines, user_lines,
    },
    timeline_cache::{
        CachedItemLayout, NextItemKind, TimelineCacheContext, TimelineItemMeta,
        TimelineLayoutProvider, TimelineLayoutSnapshot, UserPromptDescriptor,
        ViewportResolveRequest, WidthProfile,
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

fn timeline_cache_context(state: &TuiState, width: u16) -> TimelineCacheContext {
    TimelineCacheContext {
        width,
        max_reading_width: state.max_reading_width(),
        animations: state.animations_enabled(),
        code_theme: state.code_theme(),
        show_successful_command_output: state.show_successful_command_output(),
        copy_button_position: state.copy_button_position(),
        theme: state.theme().clone(),
    }
}

fn timeline_item_meta(state: &TuiState, index: usize) -> TimelineItemMeta {
    let timeline = state.timeline();
    let animations = state.animations_enabled();
    let item = &timeline[index];
    let next_kind = match timeline.get(index + 1) {
        None => NextItemKind::None,
        Some(next) if is_compact_activity_item(next) => NextItemKind::Command,
        Some(_) => NextItemKind::Other,
    };
    let volatile = matches!(
        item,
        TimelineItem::Command {
            view: CommandView::Running { .. }
        } | TimelineItem::ToolRunning { .. }
    ) || (animations
        && matches!(item, TimelineItem::Thinking { elapsed: None, .. }))
        || matches!(
            item,
            TimelineItem::Subagent { agent_id, .. }
                if state
                    .subagent_by_id(agent_id)
                    .is_some_and(|entry| entry.phase.is_active())
        );
    let compact_pair = is_compact_activity_item(item) && next_kind == NextItemKind::Command;
    TimelineItemMeta {
        version: state.timeline_item_version(index),
        next_kind,
        volatile,
        is_user: matches!(item, TimelineItem::User { .. }),
        has_trailing_blank_estimate: index + 1 < timeline.len() && !compact_pair,
    }
}

fn build_item_width_profile(state: &TuiState, index: usize) -> WidthProfile {
    match &state.timeline()[index] {
        TimelineItem::User { text } => WidthProfile::from_text(text, 2, 3, false),
        TimelineItem::Assistant { text } => WidthProfile::from_text(text, 1, 0, true),
        TimelineItem::Thinking {
            text,
            elapsed,
            expanded,
            ..
        } => {
            if elapsed.is_some() && !expanded {
                WidthProfile::fixed(1)
            } else if elapsed.is_none() {
                WidthProfile::fixed(4)
            } else {
                WidthProfile::from_text(text, 1, 2, true)
            }
        }
        TimelineItem::Muted { .. } | TimelineItem::Subagent { .. } => WidthProfile::fixed(1),
        TimelineItem::Command { view } => match view {
            CommandView::Running { expanded: true, .. } => WidthProfile::fixed(5),
            CommandView::Running {
                expanded: false, ..
            } => WidthProfile::fixed(1),
            CommandView::Finished {
                preview, expanded, ..
            } => {
                if *expanded {
                    WidthProfile::fixed(1 + preview.lines.len() + usize::from(preview.truncated))
                } else {
                    WidthProfile::fixed(1)
                }
            }
        },
        TimelineItem::ToolRunning { .. } => WidthProfile::fixed(1),
        TimelineItem::Read {
            preview, expanded, ..
        } => {
            if *expanded {
                WidthProfile::fixed(1 + preview.lines.len() + usize::from(preview.truncated))
            } else {
                WidthProfile::fixed(1)
            }
        }
        TimelineItem::LocalCommand { body, .. } => WidthProfile::from_text(body, 1, 1, true),
        TimelineItem::Expanded { body, expanded, .. } => {
            if *expanded {
                WidthProfile::from_text(body, 1, 2, true)
            } else {
                WidthProfile::fixed(1)
            }
        }
        TimelineItem::Diagnostic { body, .. } => {
            let reason = body
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("run failed");
            WidthProfile::from_text(reason, 0, 0, false)
        }
        TimelineItem::Patch { changes, expanded } => {
            if *expanded {
                let rows = changes
                    .iter()
                    .map(|change| 2 + change.lines.len() + usize::from(change.omitted_lines > 0))
                    .sum::<usize>();
                WidthProfile::fixed(rows.max(1))
            } else {
                WidthProfile::fixed(changes.len().max(1))
            }
        }
    }
}

struct StateLayoutProvider<'a> {
    state: &'a TuiState,
    region: Rect,
    latest_user_index: Option<usize>,
    has_task_dock: bool,
}

impl TimelineLayoutProvider for StateLayoutProvider<'_> {
    fn item_meta(&self, index: usize) -> TimelineItemMeta {
        timeline_item_meta(self.state, index)
    }

    fn build_width_profile(&self, index: usize) -> WidthProfile {
        build_item_width_profile(self.state, index)
    }

    fn compute_scroll(&self, starts: &[usize], heights: &[usize]) -> usize {
        timeline_scroll_start_from_geometry(
            self.state,
            starts,
            heights,
            self.latest_user_index,
            self.region,
        )
    }

    fn sticky_user_for_scroll(
        &self,
        prompts: &[UserPromptDescriptor],
        scroll: usize,
    ) -> Option<usize> {
        pinned_user_index_for_scroll(prompts, scroll, self.has_task_dock)
    }

    fn compute_item(&self, index: usize) -> CachedItemLayout {
        compute_item_layout(self.state, index, self.region.width)
    }
}

pub(super) fn timeline_layout(state: &TuiState, region: Rect) -> TimelineLayoutSnapshot {
    let timeline = state.timeline();
    let context = timeline_cache_context(state, region.width);
    let is_follow_bottom = !state.is_timeline_detached()
        && state.timeline_scroll_offset() == 0
        && state.timeline_scroll_animation_row().is_none();
    let provider = StateLayoutProvider {
        state,
        region,
        latest_user_index: timeline
            .iter()
            .rposition(|item| matches!(item, TimelineItem::User { .. })),
        has_task_dock: state.task_dock().counts(state.subagents()).total > 0,
    };
    let extra_required = state
        .comment_highlights()
        .into_iter()
        .map(|highlight| highlight.item_index)
        .collect::<Vec<_>>();

    state.timeline_cache().resolve_for_viewport(
        ViewportResolveRequest {
            context,
            len: timeline.len(),
            viewport_height: region.height,
            is_follow_bottom,
            extra_required: &extra_required,
        },
        &provider,
    )
}

/// Materializes cold/evicted items overlapping `[start_row .. end_row)` into an
/// active selection snapshot so mouse-drag autoscroll and copy across evicted
/// history remain seamless without mutating already-materialized items.
pub(crate) fn materialize_snapshot_rows(
    state: &TuiState,
    snapshot: &mut TimelineLayoutSnapshot,
    width: u16,
    start_row: usize,
    end_row: usize,
    mut on_height_delta: impl FnMut(usize, isize),
) {
    let context = timeline_cache_context(state, width);
    let len = state.timeline().len();
    let max_iters = len.min(64).saturating_add(2);
    for _ in 0..max_iters {
        let missing = snapshot.unmaterialized_in_row_range(start_row, end_row);
        if missing.is_empty() {
            return;
        }
        let mut any_patched = false;
        for index in missing {
            if index >= len {
                continue;
            }
            let meta = timeline_item_meta(state, index);
            let layout = state
                .timeline_cache()
                .materialize_item(&context, index, meta, |idx| {
                    compute_item_layout(state, idx, width)
                });
            if let Some((old_end, delta)) = snapshot.patch_materialized_item(index, layout) {
                any_patched = true;
                if delta != 0 {
                    on_height_delta(old_end, delta);
                }
            }
        }
        if !any_patched {
            return;
        }
    }
}

fn compute_item_layout(state: &TuiState, index: usize, width: u16) -> CachedItemLayout {
    let timeline = state.timeline();
    let item = &timeline[index];
    let mut item_targets = Vec::new();
    let mut item_link_targets = Vec::new();
    let item_rows = match item {
        TimelineItem::User { text } => {
            let (rows, links) = user_lines(state, text, width);
            item_link_targets = links;
            rows
        }
        TimelineItem::Assistant { text } => {
            let rendered = assistant_lines(state, text, index, width);
            item_targets = rendered.copy_targets;
            item_link_targets = rendered.link_targets;
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
            item_link_targets = rendered.link_targets;
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
        TimelineItem::Diagnostic { title, body } => {
            let (lines, links) = diagnostic_lines(state, title, body, width);
            item_link_targets = links;
            lines
                .into_iter()
                .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
                .collect()
        }
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
    let mut link_targets = Vec::with_capacity(item_link_targets.len());
    let mut item_targets = item_targets.into_iter().peekable();
    let mut item_link_targets = item_link_targets.into_iter().peekable();
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
        while item_link_targets
            .peek()
            .is_some_and(|target| target.line_index == line_index)
        {
            if let Some(mut target) = item_link_targets.next() {
                target.line_index = rows.len();
                link_targets.push(target);
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
    let mut added_post_links = false;
    for (row_index, row) in rows.iter_mut().enumerate() {
        let count = wrapped_line_count(std::slice::from_ref(&row.display), width).max(1);
        if count == 1 {
            let extra = crate::tui::links::linkify_rendered_line(
                &mut row.display,
                row_index,
                &link_targets,
            );
            if !extra.is_empty() {
                link_targets.extend(extra);
                added_post_links = true;
            }
        }
        let next = local_row_starts
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(count);
        local_row_starts.push(next);
    }
    if added_post_links {
        link_targets.sort_by_key(|target| (target.line_index, target.column));
    }

    CachedItemLayout::new(rows, local_row_starts, copy_targets, link_targets)
}

fn max_timeline_scroll_from_starts(
    starts: &[usize],
    latest_user_index: Option<usize>,
    visible: usize,
) -> usize {
    let total_rows = starts.last().copied().unwrap_or(0);
    let bottom = total_rows.saturating_sub(visible);
    let latest_user_start = latest_user_index
        .and_then(|index| starts.get(index).copied())
        .unwrap_or(0);
    bottom.max(latest_user_start)
}

fn max_timeline_scroll(
    state: &TuiState,
    timeline: &TimelineLayoutSnapshot,
    visible: usize,
) -> usize {
    let latest_user_index = state
        .timeline()
        .iter()
        .rposition(|item| matches!(item, TimelineItem::User { .. }));
    max_timeline_scroll_from_starts(&timeline.item_visual_starts, latest_user_index, visible)
}

fn timeline_scroll_start_from_geometry(
    state: &TuiState,
    starts: &[usize],
    heights: &[usize],
    latest_user_index: Option<usize>,
    region: Rect,
) -> usize {
    if let Some(anchor) = state.timeline_anchor()
        && let Some(&start) = starts.get(anchor.item_index)
        && let Some(&item_rows) = heights.get(anchor.item_index)
    {
        start.saturating_add(anchor.row.min(item_rows.saturating_sub(1)))
    } else if let Some(review_index) = state.timeline_review_user_index()
        && let Some(&start) = starts.get(review_index)
    {
        start
    } else {
        let max_scroll =
            max_timeline_scroll_from_starts(starts, latest_user_index, usize::from(region.height));
        if state.timeline_scroll_offset() == 0
            && let Some(animated_row) = state.timeline_scroll_animation_row()
        {
            return animated_row.min(max_scroll);
        }
        max_scroll.saturating_sub(state.timeline_scroll_offset())
    }
}

pub(super) fn timeline_scroll_start(
    state: &TuiState,
    timeline: &TimelineLayoutSnapshot,
    region: Rect,
) -> usize {
    if let Some(anchor) = state.timeline_anchor()
        && let Some(&start) = timeline.item_visual_starts.get(anchor.item_index)
        && anchor.item_index < timeline.items.len()
    {
        let item_rows = timeline.item_height(anchor.item_index);
        start.saturating_add(anchor.row.min(item_rows.saturating_sub(1)))
    } else if let Some(review_index) = state.timeline_review_user_index()
        && let Some(&start) = timeline.item_visual_starts.get(review_index)
    {
        start
    } else {
        let max_scroll = max_timeline_scroll(state, timeline, usize::from(region.height));
        if state.timeline_scroll_offset() == 0
            && let Some(animated_row) = state.timeline_scroll_animation_row()
        {
            return animated_row.min(max_scroll);
        }
        max_scroll.saturating_sub(state.timeline_scroll_offset())
    }
}

pub(crate) fn prepare_timeline_viewport(state: &mut TuiState, region: Rect) {
    let content = timeline_content_region(region);
    if content.is_empty() {
        return;
    }
    let timeline = timeline_layout(state, content);
    let max_scroll = max_timeline_scroll(state, &timeline, usize::from(content.height));
    if !state.is_timeline_detached() {
        state.sync_followed_timeline_scroll(max_scroll);
        return;
    }
    let scroll = timeline_scroll_start(state, &timeline, content);
    state.record_timeline_last_scroll(scroll);
    if let Some(anchor) = timeline.anchor_at(scroll) {
        state.record_timeline_viewport(anchor, max_scroll.saturating_sub(scroll));
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
