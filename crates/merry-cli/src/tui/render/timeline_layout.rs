//! Builds the transcript layout shared by drawing, hit testing, and selection.

use super::timeline::{
    assistant_lines, command_lines, compact_patch_lines, diagnostic_lines, expanded_timeline_lines,
    local_command_lines, muted_lines, user_lines,
};
use crate::tui::{
    copy_controls::CopyTarget,
    state::{TimelineAnchor, TimelineItem, TuiState},
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

pub(super) struct TimelineLayout {
    pub(super) rows: Vec<TranscriptRow>,
    pub(super) review_logical_start: Option<usize>,
    pub(super) item_starts: Vec<usize>,
    pub(super) row_starts: Vec<usize>,
    pub(super) copy_targets: Vec<CopyTarget>,
}

impl TimelineLayout {
    pub(super) fn total_rows(&self) -> usize {
        self.row_starts.last().copied().unwrap_or(0)
    }
}

pub(super) struct TimelineViewport {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) scroll: u16,
}

pub(super) fn timeline_layout(state: &TuiState, region: Rect) -> TimelineLayout {
    let mut rows = Vec::new();
    let mut review_logical_start = None;
    let mut item_starts = Vec::new();
    let mut copy_targets = Vec::new();
    for (index, item) in state.timeline().iter().enumerate() {
        item_starts.push(rows.len());
        if state.timeline_review_user_index() == Some(index) {
            review_logical_start = Some(rows.len());
        }
        let mut item_targets = Vec::new();
        let item_rows = match item {
            TimelineItem::User { text, lane } => user_lines(state, text, *lane)
                .into_iter()
                .map(|line| TranscriptRow::new(line, SelectionPolicy::StripPrefix(2)))
                .collect::<Vec<_>>(),
            TimelineItem::Assistant { text } => {
                let rendered = assistant_lines(state, text, index, region.width);
                item_targets = rendered.copy_targets;
                rendered.rows
            }
            TimelineItem::Muted { title, detail } => muted_lines(state, title, detail)
                .into_iter()
                .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
                .collect(),
            TimelineItem::Command { view } => command_lines(state, view, region.width)
                .into_iter()
                .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
                .collect(),
            TimelineItem::LocalCommand { title, body } => {
                let rendered = local_command_lines(state, title, body, region.width);
                item_targets = rendered.copy_targets;
                rendered.rows
            }
            TimelineItem::Expanded { title, body } => {
                expanded_timeline_lines(state, title, body, region.width)
                    .into_iter()
                    .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
                    .collect()
            }
            TimelineItem::Diagnostic { title, body } => {
                diagnostic_lines(state, title, body, region.width)
                    .into_iter()
                    .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
                    .collect()
            }
            TimelineItem::Patch { changes } => compact_patch_lines(state, changes)
                .into_iter()
                .map(|line| TranscriptRow::new(line, SelectionPolicy::Keep))
                .collect(),
        };
        let compact_commands = item_rows.len() == 1
            && matches!(item, TimelineItem::Command { .. })
            && matches!(
                state.timeline().get(index + 1),
                Some(TimelineItem::Command { .. })
            );
        let item_rows = spaced_timeline_item(
            item_rows,
            index + 1 < state.timeline().len() && !compact_commands,
        );
        let mut item_targets = item_targets.into_iter().peekable();
        for (line_index, row) in item_rows.into_iter().enumerate() {
            if item_targets
                .peek()
                .is_some_and(|target| target.line_index == line_index)
                && let Some(mut target) = item_targets.next()
            {
                target.line_index = rows.len();
                copy_targets.push(target);
            }
            rows.extend(
                split_oversized_timeline_line(row.display)
                    .into_iter()
                    .map(|display| TranscriptRow::new(display, row.selection)),
            );
        }
    }
    let mut row_starts: Vec<usize> = Vec::with_capacity(rows.len().saturating_add(1));
    row_starts.push(0);
    for row in &rows {
        let count = wrapped_line_count(std::slice::from_ref(&row.display), region.width).max(1);
        let next = row_starts
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(count);
        row_starts.push(next);
    }
    TimelineLayout {
        rows,
        review_logical_start,
        item_starts,
        row_starts,
        copy_targets,
    }
}

pub(super) fn timeline_viewport(
    state: &TuiState,
    timeline: TimelineLayout,
    region: Rect,
) -> TimelineViewport {
    let mut scroll = timeline_scroll_start(state, &timeline, region);
    let mut rows = timeline.rows;
    let row_starts = timeline.row_starts;
    let max_scroll = usize::from(u16::MAX);
    if scroll > max_scroll {
        // Paragraph scroll is u16; remove complete prefix lines in one linear pass.
        let rows_to_drop = scroll - max_scroll;
        let mut dropped_lines = 0;
        let mut dropped_rows = 0;
        for (index, _) in rows.iter().enumerate() {
            if dropped_rows >= rows_to_drop {
                break;
            }
            dropped_rows += row_starts[index + 1].saturating_sub(row_starts[index]);
            dropped_lines += 1;
        }
        if dropped_lines > 0 {
            rows.drain(..dropped_lines);
            scroll = scroll.saturating_sub(dropped_rows);
        }
    }

    TimelineViewport {
        lines: rows.into_iter().map(|row| row.display).collect(),
        scroll: u16::try_from(scroll).unwrap_or(u16::MAX),
    }
}

pub(super) fn timeline_scroll_start(
    state: &TuiState,
    timeline: &TimelineLayout,
    region: Rect,
) -> usize {
    if let Some(anchor) = state.timeline_anchor()
        && let Some(&start) = timeline.item_starts.get(anchor.item_index)
    {
        let end = timeline
            .item_starts
            .get(anchor.item_index + 1)
            .copied()
            .unwrap_or(timeline.rows.len());
        let item_rows = timeline.row_starts[end].saturating_sub(timeline.row_starts[start]);
        timeline.row_starts[start].saturating_add(anchor.row.min(item_rows.saturating_sub(1)))
    } else if let Some(logical_start) = timeline.review_logical_start {
        timeline.row_starts[logical_start]
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
    for (index, &start) in timeline.item_starts.iter().enumerate() {
        let end = timeline
            .item_starts
            .get(index + 1)
            .copied()
            .unwrap_or(timeline.rows.len());
        let start_row = timeline.row_starts[start];
        let rows = timeline.row_starts[end].saturating_sub(start_row);
        if scroll < start_row.saturating_add(rows) {
            state.record_timeline_viewport(
                TimelineAnchor::new(index, scroll.saturating_sub(start_row)),
                total
                    .saturating_sub(usize::from(content.height))
                    .saturating_sub(scroll),
            );
            break;
        }
    }
}

pub(crate) fn timeline_content_region(region: Rect) -> Rect {
    Block::default()
        .borders(Borders::BOTTOM)
        .padding(Padding::left(1))
        .inner(region)
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
