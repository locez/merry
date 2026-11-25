//! Pointer hits on the timeline, including the pinned user header.

use super::activity::thinking_fold_hit;
use super::sticky::{split_timeline_content, timeline_scroll_split};
use super::task_dock::{TaskDockPointer, adjust_body_scroll, dock_mouse_down, split_dock_region};
use super::timeline::command_fold_hit;
use super::timeline_cache::TimelineLayoutSnapshot;
use super::timeline_layout::{timeline_content_region, timeline_layout, timeline_scroll_start};
use crate::tui::{
    copy_controls::{CopyContent, CopyTextError, validate_copy_text},
    links::HoveredLink,
    state::{TimelineItem, TuiState},
    text_interaction::TextSelection,
};
use ratatui::layout::{Position, Rect, Size};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TimelineMouseDown {
    None,
    Copy(String),
    CopyTooLarge,
    /// Opens or closes a finished command without starting text selection.
    ToggleCommand(usize),
    /// Opens or closes a finished thinking block without starting text selection.
    ToggleThinking(usize),
    /// Opens the interactive Subagent Takeover View for the clicked subagent card.
    InspectSubagent(String),
    /// Handles a click inside the persistent Background Tasks & Subagents dock.
    TaskDock(TaskDockPointer),
    /// Handles a click inside the inline Todo checklist dock.
    TodoDock(crate::tui::todo_dock::TodoDockPointer),
    Select(TextSelection),
}

/// Resolves a timeline press from one cached layout snapshot.
pub(crate) fn timeline_mouse_down(
    state: &TuiState,
    region: Rect,
    terminal_size: Size,
    position: Position,
) -> TimelineMouseDown {
    let content = timeline_content_region(region);
    if !region.contains(position) || content.is_empty() {
        return TimelineMouseDown::None;
    }
    let timeline = timeline_layout(state, content);
    let scroll = timeline_scroll_start(state, &timeline, content);
    let split = timeline_scroll_split(state, &timeline, content.height, scroll);
    let (_, after_header) = split_timeline_content(content, split.header_rows);
    let (dock_rect, after_task_dock) = split_dock_region(state, after_header);
    if let Some(pointer) = dock_mouse_down(state, dock_rect, position) {
        return TimelineMouseDown::TaskDock(pointer);
    }
    if dock_rect.contains(position) {
        return TimelineMouseDown::None;
    }
    let (todo_rect, body) = crate::tui::todo_dock::split_todo_dock_region(state, after_task_dock);
    if let Some(pointer) = crate::tui::todo_dock::todo_dock_mouse_down(state, todo_rect, position) {
        return TimelineMouseDown::TodoDock(pointer);
    }
    if todo_rect.contains(position) {
        return TimelineMouseDown::None;
    }
    let body_scroll = adjust_body_scroll(
        split.body_scroll,
        after_header.height,
        body.height,
        timeline.total_rows(),
    );
    if let Some(index) = thinking_fold_hit(state, body, &timeline, body_scroll, position) {
        return TimelineMouseDown::ToggleThinking(index);
    }
    if let Some(index) = command_fold_hit(state, body, &timeline, body_scroll, position) {
        return TimelineMouseDown::ToggleCommand(index);
    }
    if body.contains(position)
        && let Some(agent_id) = subagent_card_hit(state, body, &timeline, body_scroll, position)
    {
        return TimelineMouseDown::InspectSubagent(agent_id);
    }
    if body.contains(position) {
        match copy_target_text(state, &timeline, body, body_scroll, position) {
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
    match TextSelection::new(timeline, body, terminal_size, body_scroll, position) {
        Some(selection) => TimelineMouseDown::Select(selection),
        None => TimelineMouseDown::None,
    }
}

/// Timeline body that text selection uses. A pinned user message and optional
/// task dock sit above it.
pub(crate) fn timeline_text_area(state: &TuiState, region: Rect) -> Rect {
    let content = timeline_content_region(region);
    if content.is_empty() {
        return content;
    }
    let timeline = timeline_layout(state, content);
    let scroll = timeline_scroll_start(state, &timeline, content);
    let split = timeline_scroll_split(state, &timeline, content.height, scroll);
    let (_, after_header) = split_timeline_content(content, split.header_rows);
    let (_, after_task_dock) = split_dock_region(state, after_header);
    crate::tui::todo_dock::split_todo_dock_region(state, after_task_dock).1
}

/// Returns the screen rectangle occupied by the inline task dock, if visible.
pub(crate) fn timeline_task_dock_rect(state: &TuiState, region: Rect) -> Rect {
    let content = timeline_content_region(region);
    if content.is_empty() {
        return Rect::default();
    }
    let timeline = timeline_layout(state, content);
    let scroll = timeline_scroll_start(state, &timeline, content);
    let split = timeline_scroll_split(state, &timeline, content.height, scroll);
    let (_, after_header) = split_timeline_content(content, split.header_rows);
    split_dock_region(state, after_header).0
}

/// Returns the screen rectangle occupied by the inline todo dock, if visible.
pub(crate) fn timeline_todo_dock_rect(state: &TuiState, region: Rect) -> Rect {
    let content = timeline_content_region(region);
    if content.is_empty() {
        return Rect::default();
    }
    let timeline = timeline_layout(state, content);
    let scroll = timeline_scroll_start(state, &timeline, content);
    let split = timeline_scroll_split(state, &timeline, content.height, scroll);
    let (_, after_header) = split_timeline_content(content, split.header_rows);
    let (_, after_task_dock) = split_dock_region(state, after_header);
    crate::tui::todo_dock::split_todo_dock_region(state, after_task_dock).0
}

fn copy_target_text(
    state: &TuiState,
    timeline: &TimelineLayoutSnapshot,
    content: Rect,
    scroll: usize,
    position: Position,
) -> Result<Option<String>, CopyTextError> {
    let clicked_row = scroll.saturating_add(usize::from(position.y - content.y));
    let column = position.x - content.x;
    let Some(item_index) = timeline.item_at_visual(clicked_row) else {
        return Ok(None);
    };
    let Some(item) = timeline.item_layout(item_index) else {
        return Ok(None);
    };
    let item_start = timeline.item_visual_starts[item_index];
    for target in &item.copy_targets {
        let Some(&local_row) = item.local_row_starts.get(target.line_index) else {
            continue;
        };
        let target_row = item_start.saturating_add(local_row);
        if target_row > clicked_row {
            break;
        }
        if target_row == clicked_row
            && (target.column..target.column.saturating_add(target.width)).contains(&column)
        {
            return match &target.content {
                CopyContent::Text(text) => validate_copy_text(text).map(Some),
                CopyContent::AssistantMessage(index) => match state.timeline().get(*index) {
                    Some(TimelineItem::Assistant { text }) => validate_copy_text(text).map(Some),
                    _ => Ok(None),
                },
            };
        }
    }
    Ok(None)
}

/// Resolves a hyperlink under `position` inside the timeline body.
pub(crate) fn timeline_link_at(
    state: &TuiState,
    region: Rect,
    position: Position,
) -> Option<HoveredLink> {
    let content = timeline_content_region(region);
    if !region.contains(position) || content.is_empty() {
        return None;
    }
    let timeline = timeline_layout(state, content);
    let scroll = timeline_scroll_start(state, &timeline, content);
    let split = timeline_scroll_split(state, &timeline, content.height, scroll);
    let (_, after_header) = split_timeline_content(content, split.header_rows);
    let (dock_rect, after_task_dock) = split_dock_region(state, after_header);
    if dock_rect.contains(position) {
        return None;
    }
    let (todo_rect, body) = crate::tui::todo_dock::split_todo_dock_region(state, after_task_dock);
    if todo_rect.contains(position) || !body.contains(position) {
        return None;
    }
    let body_scroll = adjust_body_scroll(
        split.body_scroll,
        after_header.height,
        body.height,
        timeline.total_rows(),
    );
    link_target_at(&timeline, body, body_scroll, position)
}

/// Validates that `hovered` still corresponds to a visible link in `body` at
/// `body_scroll` and returns its current screen row `y`.
pub(super) fn hovered_link_screen_y(
    timeline: &TimelineLayoutSnapshot,
    body: Rect,
    body_scroll: usize,
    hovered: &HoveredLink,
) -> Option<u16> {
    let visible_end = body_scroll.saturating_add(usize::from(body.height));
    if hovered.document_row < body_scroll || hovered.document_row >= visible_end {
        return None;
    }
    let item = timeline.item_layout(hovered.item_index)?;
    let item_start = *timeline.item_visual_starts.get(hovered.item_index)?;
    let still_present = item.link_targets.iter().any(|target| {
        item.local_row_starts
            .get(target.line_index)
            .is_some_and(|&local_row| {
                item_start.saturating_add(local_row) == hovered.document_row
                    && target.column == hovered.column
                    && target.width == hovered.width
                    && target.url == hovered.url
            })
    });
    if !still_present {
        return None;
    }
    let rel_y = u16::try_from(hovered.document_row.saturating_sub(body_scroll)).ok()?;
    Some(body.y.saturating_add(rel_y))
}

fn link_target_at(
    timeline: &TimelineLayoutSnapshot,
    body: Rect,
    scroll: usize,
    position: Position,
) -> Option<HoveredLink> {
    let document_row = scroll.saturating_add(usize::from(position.y - body.y));
    let column = position.x - body.x;
    let item_index = timeline.item_at_visual(document_row)?;
    let item = timeline.item_layout(item_index)?;
    let item_start = timeline.item_visual_starts[item_index];
    for target in &item.link_targets {
        let Some(&local_row) = item.local_row_starts.get(target.line_index) else {
            continue;
        };
        let target_row = item_start.saturating_add(local_row);
        if target_row > document_row {
            break;
        }
        if target_row == document_row
            && (target.column..target.column.saturating_add(target.width)).contains(&column)
        {
            return Some(HoveredLink::new(
                target.url.clone(),
                target.column,
                target.width,
                document_row,
                item_index,
            ));
        }
    }
    None
}

fn subagent_card_hit(
    state: &TuiState,
    content: Rect,
    timeline: &TimelineLayoutSnapshot,
    scroll: usize,
    position: Position,
) -> Option<String> {
    let clicked_row = scroll.saturating_add(usize::from(position.y.saturating_sub(content.y)));
    let index = timeline.item_at_visual(clicked_row)?;
    let (start, end) = timeline.item_content_visual_range(index)?;
    if clicked_row < start || clicked_row >= end {
        return None;
    }
    match state.timeline().get(index)? {
        TimelineItem::Subagent { agent_id, .. } => Some(agent_id.clone()),
        _ => None,
    }
}
