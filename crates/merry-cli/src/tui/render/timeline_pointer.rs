//! Pointer hits on the timeline, including the pinned user header.

use super::activity::{ActivityLayout, thinking_fold_hit};
use super::sticky::{split_timeline_content, timeline_scroll_split};
use super::timeline::blank_logical_lines;
use super::timeline::command_fold_hit;
use super::timeline_layout::{timeline_content_region, timeline_layout, timeline_scroll_start};
use crate::tui::{
    copy_controls::{CopyContent, CopyTextError, validate_copy_text},
    state::{TimelineItem, TuiState},
    text_interaction::TextSelection,
};
use ratatui::layout::{Position, Rect};

use super::timeline_layout::TimelineLayout;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TimelineMouseDown {
    None,
    Copy(String),
    CopyTooLarge,
    /// Opens or closes a finished command without starting text selection.
    ToggleCommand(usize),
    /// Opens or closes a finished thinking block without starting text selection.
    ToggleThinking(usize),
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
    let scroll = timeline_scroll_start(state, &timeline, content);
    let split = timeline_scroll_split(
        state,
        &timeline.item_starts,
        &timeline.row_starts,
        &timeline.rows,
        content.height,
        scroll,
    );
    let (_, body) = split_timeline_content(content, split.header_rows);
    let blank_lines = blank_logical_lines(&timeline);
    if let Some(index) = thinking_fold_hit(
        state,
        body,
        &ActivityLayout {
            item_starts: &timeline.item_starts,
            row_starts: &timeline.row_starts,
            blank_lines: &blank_lines,
        },
        split.body_scroll,
        position,
    ) {
        return TimelineMouseDown::ToggleThinking(index);
    }
    if let Some(index) = command_fold_hit(state, body, &timeline, split.body_scroll, position) {
        return TimelineMouseDown::ToggleCommand(index);
    }
    if body.contains(position) {
        match copy_target_text(state, &timeline, body, split.body_scroll, position) {
            Ok(Some(text)) => return TimelineMouseDown::Copy(text),
            Ok(None) => {}
            Err(CopyTextError::TooLarge) => return TimelineMouseDown::CopyTooLarge,
        }
    }
    if position.y < body.y || position.y >= body.bottom() {
        return TimelineMouseDown::None;
    }
    let position = Position::new(
        position.x.clamp(body.x, body.right().saturating_sub(1)),
        position.y,
    );
    match TextSelection::new(
        timeline.rows,
        timeline.item_starts,
        timeline.row_starts,
        body,
        split.body_scroll,
        position,
    ) {
        Some(selection) => TimelineMouseDown::Select(selection),
        None => TimelineMouseDown::None,
    }
}

/// Timeline body that text selection uses. A pinned user message sits above it.
pub(crate) fn timeline_text_area(state: &TuiState, region: Rect) -> Rect {
    let content = timeline_content_region(region);
    if content.is_empty() {
        return content;
    }
    let timeline = timeline_layout(state, content);
    let scroll = timeline_scroll_start(state, &timeline, content);
    let split = timeline_scroll_split(
        state,
        &timeline.item_starts,
        &timeline.row_starts,
        &timeline.rows,
        content.height,
        scroll,
    );
    split_timeline_content(content, split.header_rows).1
}

fn copy_target_text(
    state: &TuiState,
    timeline: &TimelineLayout,
    content: Rect,
    scroll: usize,
    position: Position,
) -> Result<Option<String>, CopyTextError> {
    let clicked_row = scroll.saturating_add(usize::from(position.y - content.y));
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
