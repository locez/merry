//! Pointer targets for the cockpit.
//!
//! Hit rectangles come from the same layout functions as the renderers, so a
//! click lands on the control the current state would draw.

use crate::tui::{
    keymap::KeyAction,
    layout::{BottomPaneHeights, TimelineRects, cockpit_layout},
    overlay_render::overlay_pointer,
    render::{self, STATUS_HEIGHT},
    state::TuiState,
};
use ratatui::{
    layout::{Position, Rect, Size},
    style::Modifier,
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{text_wrap::semantic_style, theme::SemanticColor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusAction {
    Commands,
    Agents,
    PlanMode,
    Plan,
    Follow,
    Stop,
    Comment,
    SendComments,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverlayPointer {
    CloseModal,
    Palette(usize),
    Skill(usize),
    Settings(crate::tui::settings_render::SettingsPointer),
    PlanView(crate::tui::plan_view::PlanViewPointer),
    PermissionAllow,
    PermissionReject,
    DialogClose,
    ProviderWorkbench(crate::tui::provider_render::ProviderWorkbenchPointer),
    Model(usize),
    ManualModel,
    Reasoning(usize),
    CommandPrevious,
    CommandNext,
    CommandCopyCommand,
    CommandCopyOutput,
    CommandClose,
    CommandSelect(usize),
    CommandFocusPane(crate::tui::command_details::StreamPane),
    Subagent(crate::tui::subagent_view::SubagentTakeoverPointer),
    BackgroundTask(crate::tui::task_dock::BackgroundTaskPointer),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PointerTarget {
    Passthrough,
    Consumed,
    Model,
    Status(StatusAction),
    Composer,
    PendingQueue(crate::tui::render::PendingQueuePointer),
    Completion(usize),
    Overlay(OverlayPointer),
}

pub(crate) fn pointer_target(state: &TuiState, size: Size, position: Position) -> PointerTarget {
    if state.overlay().is_some() {
        let area = Rect::new(0, 0, size.width, size.height);
        return match overlay_pointer(state, area, position) {
            Some(pointer) => PointerTarget::Overlay(pointer),
            None => PointerTarget::Consumed,
        };
    }

    let rects = cockpit_rects(size, state);
    if let Some(action) = status_action_at(state, rects.status, position) {
        return PointerTarget::Status(action);
    }
    if header_model_rect(rects.header, state).is_some_and(|model| model.contains(position)) {
        return PointerTarget::Model;
    }
    if let Some(queue) = rects.queue
        && queue.contains(position)
    {
        return render::pending_queue_pointer_at(state, queue, position)
            .map(PointerTarget::PendingQueue)
            .unwrap_or(PointerTarget::Consumed);
    }
    if rects.completion.contains(position) {
        return completion_index(state, rects.completion, position)
            .map(PointerTarget::Completion)
            .unwrap_or(PointerTarget::Consumed);
    }
    if rects.input.contains(position) {
        return PointerTarget::Composer;
    }
    PointerTarget::Passthrough
}

fn cockpit_rects(size: Size, state: &TuiState) -> TimelineRects {
    let area = Rect::new(0, 0, size.width, size.height);
    let pane_heights = render::pane_heights_for_area(state, area);
    cockpit_layout(
        area,
        BottomPaneHeights {
            queue: pane_heights.queue,
            completion: pane_heights.completion,
            input: pane_heights.input,
            status: STATUS_HEIGHT,
        },
    )
}

pub(crate) fn status_actions(state: &TuiState) -> Vec<StatusAction> {
    let mut actions = vec![StatusAction::Commands, StatusAction::Comment];
    if !state.subagents().is_empty() {
        actions.push(StatusAction::Agents);
    }
    if state.plan().mode() != merry_core::PlanModeState::Inactive {
        actions.push(StatusAction::PlanMode);
    }
    if state.plan().has_plans() {
        actions.push(StatusAction::Plan);
    }
    if state.is_timeline_detached() {
        actions.push(StatusAction::Follow);
    }
    if state.can_interrupt_run() {
        actions.push(StatusAction::Stop);
    }
    if state.can_send_comments() {
        actions.push(StatusAction::SendComments);
    }
    actions
}

pub(crate) fn status_action_label(state: &TuiState, action: &StatusAction) -> String {
    match action {
        StatusAction::Commands => "commands".to_owned(),
        StatusAction::Agents => {
            let count = state.subagents().len();
            if count == 1 {
                "1 agent".to_owned()
            } else {
                format!("{count} agents")
            }
        }
        StatusAction::PlanMode => match state.plan().mode() {
            merry_core::PlanModeState::Pending => "PLAN (pending)".to_owned(),
            merry_core::PlanModeState::Active => "PLAN".to_owned(),
            merry_core::PlanModeState::ExitPending => "PLAN (exiting)".to_owned(),
            merry_core::PlanModeState::Inactive => "plan mode".to_owned(),
        },
        StatusAction::Plan => {
            if state.plan().is_awaiting_approval() {
                "Review Plan".to_owned()
            } else {
                let count = state.plan().plans().len();
                if count == 1 {
                    "1 plan".to_owned()
                } else {
                    format!("{count} plans")
                }
            }
        }
        StatusAction::Follow => "follow".to_owned(),
        StatusAction::Stop => "stop".to_owned(),
        StatusAction::Comment => comment_chip_label(state),
        StatusAction::SendComments => "send".to_owned(),
    }
}

fn comment_chip_label(state: &TuiState) -> String {
    let verb = if state.is_commenting() {
        "commenting"
    } else {
        "comment"
    };
    match state.keymap().binding_label_for(KeyAction::ToggleComment) {
        Some(key) => format!("{key} {verb}"),
        None => verb.to_owned(),
    }
}

pub(crate) fn status_chips(
    state: &TuiState,
    region: Rect,
    actions: &[StatusAction],
) -> Vec<(StatusAction, Rect)> {
    if region.height == 0 || region.width == 0 {
        return Vec::new();
    }
    let right_pad: u16 = if region.width >= 40 { 1 } else { 0 };
    let usable_width = region.width.saturating_sub(right_pad);
    let mut shown = actions.to_vec();
    while !shown.is_empty()
        && chip_row_width(&chip_labels(state, &shown)) > usize::from(usable_width)
    {
        shown.remove(0);
    }
    let labels = chip_labels(state, &shown);
    let width = chip_row_width(&labels);
    let origin_x = region
        .x
        .saturating_add(usable_width.saturating_sub(u16::try_from(width).unwrap_or(usable_width)));
    chip_rects(origin_x, region.y, &labels)
        .into_iter()
        .zip(shown)
        .map(|(rect, action)| (action, rect))
        .collect()
}

pub(crate) fn status_action_at(
    state: &TuiState,
    region: Rect,
    position: Position,
) -> Option<StatusAction> {
    if state.clipboard_feedback().is_some() {
        return None;
    }
    status_chips(state, region, &status_actions(state))
        .into_iter()
        .find(|(_, rect)| rect.contains(position))
        .map(|(action, _)| action)
}

pub(crate) fn header_model_rect(region: Rect, state: &TuiState) -> Option<Rect> {
    if region.height == 0 || region.width == 0 {
        return None;
    }
    let model = state.model_display_label();
    let rate = state.output_rate_label();
    let layout = crate::tui::status::build_header_layout(
        state.workspace_root(),
        &model,
        state.usage(),
        &rate,
        state.is_active_run(),
        state.header_items(),
        region.width,
    );
    let (offset, width) = layout.model_range?;
    if offset >= region.width || width == 0 {
        return None;
    }
    let clamped = width.min(region.width.saturating_sub(offset));
    Some(Rect::new(
        region.x.saturating_add(offset),
        region.y,
        clamped,
        1,
    ))
}

pub(crate) fn chip_line(state: &TuiState, labels: &[impl AsRef<str>]) -> Line<'static> {
    let mut style = semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
    if let Some(bg) = state.theme().bg_color(SemanticColor::ChipBackground) {
        style = style.bg(bg);
    }
    let mut spans = Vec::new();
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(format!("[{}]", label.as_ref()), style));
    }
    Line::from(spans)
}

pub(crate) fn chip_hit(
    origin: Position,
    labels: &[impl AsRef<str>],
    position: Position,
) -> Option<usize> {
    chip_rects(origin.x, origin.y, labels)
        .into_iter()
        .position(|rect| rect.contains(position))
}

pub(crate) fn truncate_columns(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let limit = max_width.saturating_sub(1);
    let mut width = 0;
    let mut end = 0;
    for (index, character) in value.char_indices() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width + character_width > limit {
            break;
        }
        width += character_width;
        end = index + character.len_utf8();
    }
    let mut truncated = value[..end].to_owned();
    truncated.push('…');
    truncated
}

fn chip_labels(state: &TuiState, actions: &[StatusAction]) -> Vec<String> {
    actions
        .iter()
        .map(|action| status_action_label(state, action))
        .collect()
}

fn chip_rects(origin_x: u16, y: u16, labels: &[impl AsRef<str>]) -> Vec<Rect> {
    let mut x = origin_x;
    let mut rects = Vec::with_capacity(labels.len());
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(2);
        }
        let width = u16::try_from(UnicodeWidthStr::width(
            format!("[{}]", label.as_ref()).as_str(),
        ))
        .unwrap_or(u16::MAX);
        rects.push(Rect::new(x, y, width, 1));
        x = x.saturating_add(width);
    }
    rects
}

fn chip_row_width(labels: &[impl AsRef<str>]) -> usize {
    labels
        .iter()
        .map(|label| UnicodeWidthStr::width(format!("[{}]", label.as_ref()).as_str()))
        .sum::<usize>()
        .saturating_add(labels.len().saturating_sub(1).saturating_mul(2))
}

fn completion_index(state: &TuiState, region: Rect, position: Position) -> Option<usize> {
    if let Some(search) = state.history_search() {
        let index = usize::from(position.y.saturating_sub(region.y));
        let (start, end) = search.visible_window(usize::from(region.height).max(1));
        return (index < usize::from(region.height) && start + index < end).then_some(index);
    }
    let menu = state.completion_menu()?;
    let row = usize::from(position.y.saturating_sub(region.y));
    let (start, end) = menu.visible_window(usize::from(region.height));
    (row < usize::from(region.height) && start + row < end).then_some(start + row)
}
