//! Pointer, click, and mouse-wheel translation for the TUI controller.

use super::input::{
    PLAN_SCROLL_STEP, commit_pending_edit_effect, handle_key_action, input_is_known_slash,
    press_overlay,
};
use crate::tui::{
    click_target::{OverlayPointer, PointerTarget, StatusAction, pointer_target},
    command_details::CommandNavigation,
    controller::{ControllerEffect, cockpit_rects},
    copy_controls::CopyTextError,
    keymap::KeyAction,
    overlay::Overlay,
    plan_render::PlanPointer,
    state::TuiState,
    text_interaction::MouseInput,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect, Size};

fn overlay_pointer_effect(pointer: OverlayPointer, state: &mut TuiState) -> ControllerEffect {
    match pointer {
        OverlayPointer::CloseModal => press_overlay(state, KeyCode::Esc),
        OverlayPointer::Palette(index) => {
            if let Some(Overlay::CommandPalette(palette)) = state.overlay_mut() {
                palette.select(index);
            }
            press_overlay(state, KeyCode::Enter)
        }
        OverlayPointer::Setting(item) => {
            if settings_editor_open(state) {
                return ControllerEffect::None;
            }
            if let Some(Overlay::Settings(settings)) = state.overlay_mut() {
                settings.select_item(item);
            }
            press_overlay(state, KeyCode::Enter)
        }
        OverlayPointer::PlanApprove
        | OverlayPointer::DialogClose
        | OverlayPointer::PermissionAllow => press_overlay(state, KeyCode::Enter),
        OverlayPointer::PlanBack | OverlayPointer::PermissionReject => {
            press_overlay(state, KeyCode::Esc)
        }
        OverlayPointer::ProviderWorkbench(action) => {
            provider_workbench_pointer_effect(action, state)
        }
        OverlayPointer::Model(index) => {
            if let Some(Overlay::ModelPicker(picker)) = state.overlay_mut() {
                picker.select(index);
            }
            press_overlay(state, KeyCode::Enter)
        }
        OverlayPointer::ManualModel => press_overlay(state, KeyCode::Enter),
        OverlayPointer::Reasoning(index) => {
            if let Some(Overlay::ReasoningPicker(picker)) = state.overlay_mut() {
                picker.select(index);
            }
            press_overlay(state, KeyCode::Enter)
        }
        OverlayPointer::CommandPrevious => {
            crate::tui::command_details::inspect_command(state, CommandNavigation::Previous)
        }
        OverlayPointer::CommandNext => {
            crate::tui::command_details::inspect_command(state, CommandNavigation::Next)
        }
        OverlayPointer::CommandSelect(index) => {
            crate::tui::command_details::inspect_command(state, CommandNavigation::Index(index))
        }
        OverlayPointer::CommandFocusPane(pane) => {
            if let Some(Overlay::CommandDetails(details)) = state.overlay_mut() {
                details.focus_pane(pane);
            }
            ControllerEffect::None
        }
        OverlayPointer::CommandCopyCommand => press_overlay(state, KeyCode::Char('c')),
        OverlayPointer::CommandCopyOutput => press_overlay(state, KeyCode::Char('y')),
        OverlayPointer::CommandClose => press_overlay(state, KeyCode::Esc),
        OverlayPointer::Subagent(pointer) => {
            use crate::tui::subagent_view::SubagentTakeoverPointer;
            match pointer {
                SubagentTakeoverPointer::Back => {
                    state.close_overlay();
                    ControllerEffect::None
                }
                SubagentTakeoverPointer::Close => {
                    if let Some(agent_id) = state.inspected_subagent_id() {
                        state.close_subagent_from_dock(&agent_id);
                    }
                    state.close_overlay();
                    ControllerEffect::None
                }
                SubagentTakeoverPointer::Previous => {
                    state.step_subagent_inspect(-1);
                    ControllerEffect::RefreshSubagents
                }
                SubagentTakeoverPointer::Next => {
                    state.step_subagent_inspect(1);
                    ControllerEffect::RefreshSubagents
                }
                SubagentTakeoverPointer::Refresh => ControllerEffect::RefreshSubagents,
                SubagentTakeoverPointer::Stop => state
                    .inspected_subagent_id()
                    .map(ControllerEffect::CancelSubagent)
                    .unwrap_or(ControllerEffect::None),
                SubagentTakeoverPointer::SelectAgent(agent_id) => {
                    state.select_subagent_inspect(agent_id);
                    ControllerEffect::RefreshSubagents
                }
                SubagentTakeoverPointer::Composer => ControllerEffect::None,
                SubagentTakeoverPointer::TimelineToggleCommand(index) => {
                    if let Some(Overlay::SubagentInspect(view)) = state.overlay_mut() {
                        view.child_state_mut().toggle_command_expanded(index);
                    }
                    ControllerEffect::None
                }
                SubagentTakeoverPointer::TimelineToggleThinking(index) => {
                    if let Some(Overlay::SubagentInspect(view)) = state.overlay_mut() {
                        view.child_state_mut().toggle_thinking_expanded(index);
                    }
                    ControllerEffect::None
                }
                SubagentTakeoverPointer::TimelineCopy(text) => ControllerEffect::CopyText(text),
                SubagentTakeoverPointer::TimelineCopyTooLarge => ControllerEffect::CopyTextTooLarge,
            }
        }
        OverlayPointer::BackgroundTask(pointer) => {
            use crate::tui::task_dock::BackgroundTaskPointer;
            let process_id = state.inspected_background_process_id();
            match pointer {
                BackgroundTaskPointer::Close => {
                    state.close_overlay();
                    ControllerEffect::None
                }
                BackgroundTaskPointer::Refresh => process_id
                    .map(ControllerEffect::InspectBackgroundProcess)
                    .unwrap_or(ControllerEffect::None),
                BackgroundTaskPointer::Stop => process_id
                    .map(ControllerEffect::StopBackgroundProcess)
                    .unwrap_or(ControllerEffect::None),
            }
        }
    }
}

fn provider_workbench_pointer_effect(
    pointer: crate::tui::provider_render::ProviderWorkbenchPointer,
    state: &mut TuiState,
) -> ControllerEffect {
    use crate::tui::provider_render::ProviderWorkbenchPointer;
    match pointer {
        ProviderWorkbenchPointer::Close => press_overlay(state, KeyCode::Esc),
        ProviderWorkbenchPointer::SelectProvider(index) => match state.overlay_mut() {
            Some(Overlay::ProviderManager(manager)) => {
                manager.select(index);
                ControllerEffect::None
            }
            Some(Overlay::ProviderForm(_)) => {
                let items = state.provider_workbench_items().to_vec();
                state.open_provider_manager(items);
                if let Some(Overlay::ProviderManager(manager)) = state.overlay_mut() {
                    manager.select(index);
                }
                ControllerEffect::None
            }
            _ => ControllerEffect::None,
        },
        ProviderWorkbenchPointer::AddProvider => match state.overlay_mut() {
            Some(Overlay::ProviderManager(manager)) => {
                manager.start_inline_add();
                ControllerEffect::None
            }
            Some(Overlay::ProviderForm(_)) => ControllerEffect::OpenProviderForm,
            _ => ControllerEffect::None,
        },
        ProviderWorkbenchPointer::Field(field) => {
            let action = match state.overlay_mut() {
                Some(Overlay::ProviderManager(manager)) => manager.click_detail_field(field),
                Some(Overlay::ProviderForm(form)) => form.click_field(field),
                _ => return ControllerEffect::None,
            };
            crate::tui::controller_provider::provider_overlay_effect(action)
        }
        ProviderWorkbenchPointer::BrowseModels => {
            let action = match state.overlay_mut() {
                Some(Overlay::ProviderManager(manager)) => {
                    if manager.detail_form().is_some() {
                        manager.trigger_detail_model_discovery()
                    } else if let Some(item) = manager.selected_item() {
                        crate::tui::provider_overlay::ProviderOverlayAction::OpenModelPicker(
                            item.alias().to_owned(),
                        )
                    } else {
                        return ControllerEffect::None;
                    }
                }
                Some(Overlay::ProviderForm(form)) => form.trigger_model_discovery(),
                _ => return ControllerEffect::None,
            };
            crate::tui::controller_provider::provider_overlay_effect(action)
        }
        ProviderWorkbenchPointer::SetActive => {
            if let Some(Overlay::ProviderManager(manager)) = state.overlay_mut() {
                manager.focus_list();
                return press_overlay(state, KeyCode::Enter);
            }
            ControllerEffect::None
        }
        ProviderWorkbenchPointer::DeleteSelected => {
            let action = match state.overlay_mut() {
                Some(Overlay::ProviderManager(manager)) => manager.trigger_delete_selected(),
                _ => return ControllerEffect::None,
            };
            crate::tui::controller_provider::provider_overlay_effect(action)
        }
    }
}

fn scroll_active_overlay(state: &mut TuiState, up: bool, lines: usize) -> bool {
    let Some(overlay) = state.overlay_mut() else {
        return false;
    };
    match overlay {
        Overlay::PermissionReview(review) => {
            let step = u16::try_from(lines).unwrap_or(u16::MAX).max(1);
            if up {
                review.scroll_up(step);
            } else {
                review.scroll_down(step);
            }
            true
        }
        Overlay::SubagentInspect(takeover) => {
            let step = lines.max(1);
            if up {
                takeover.scroll_up(step);
            } else {
                takeover.scroll_down(step);
            }
            true
        }
        Overlay::BackgroundTaskInspect(inspector) => {
            let step = lines.max(1);
            if up {
                inspector.scroll_up(step);
            } else {
                inspector.scroll_down(step, usize::MAX / 2);
            }
            true
        }
        Overlay::CommandPalette(_)
        | Overlay::Settings(_)
        | Overlay::ProviderManager(_)
        | Overlay::ModelPicker(_)
        | Overlay::ReasoningPicker(_) => {
            let key = if up { KeyCode::Up } else { KeyCode::Down };
            let _ = overlay.handle_key(KeyEvent::new(key, KeyModifiers::NONE));
            true
        }
        _ => false,
    }
}

fn settings_editor_open(state: &TuiState) -> bool {
    state.settings_model_editor().is_some()
        || state.settings_reasoning_editor().is_some()
        || state.settings_context_window_editor().is_some()
}

fn open_model_from_header(state: &TuiState) -> ControllerEffect {
    match state.current_provider_alias() {
        Some(alias) => ControllerEffect::OpenModelPicker(alias.to_owned()),
        None => ControllerEffect::OpenProviderManager,
    }
}

fn status_click(action: StatusAction, state: &mut TuiState) -> ControllerEffect {
    if action == StatusAction::Agents {
        state.open_subagent_inspect(None);
        return ControllerEffect::RefreshSubagents;
    }
    let key_action = match action {
        StatusAction::Commands => KeyAction::OpenCommandPanel,
        StatusAction::Agents => unreachable!("handled above"),
        StatusAction::Plan => KeyAction::TogglePlan,
        StatusAction::Follow => KeyAction::FollowLatest,
        StatusAction::Stop => KeyAction::Interrupt,
        StatusAction::Comment => KeyAction::ToggleComment,
        StatusAction::SendComments => KeyAction::SendComments,
    };
    handle_key_action(key_action, state)
}

fn apply_pending_queue_pointer(
    state: &mut TuiState,
    pointer: crate::tui::render::PendingQueuePointer,
) -> ControllerEffect {
    use crate::tui::render::PendingQueuePointer;
    match pointer {
        PendingQueuePointer::Edit(id) => {
            if state.editing_pending_id() == Some(id) {
                return ControllerEffect::None;
            }
            if state.is_editing_pending() {
                return match state.switch_pending_edit(id) {
                    Some(crate::tui::state::PendingEditCommit::Update { id, text }) => {
                        ControllerEffect::UpdatePendingInput {
                            id,
                            text,
                            resume: false,
                        }
                    }
                    Some(crate::tui::state::PendingEditCommit::Remove { id }) => {
                        ControllerEffect::RemovePendingInput {
                            id,
                            resume: Some(false),
                        }
                    }
                    None => ControllerEffect::None,
                };
            }
            match state.begin_pending_edit(id) {
                Some(true) => ControllerEffect::SetPendingPaused(true),
                _ => ControllerEffect::None,
            }
        }
        PendingQueuePointer::Remove(id) => {
            let resume = if state.editing_pending_id() == Some(id) {
                state
                    .cancel_pending_edit()
                    .map(|paused_before_edit| !paused_before_edit)
            } else {
                None
            };
            ControllerEffect::RemovePendingInput { id, resume }
        }
        PendingQueuePointer::SaveEdit => commit_pending_edit_effect(state, true),
        PendingQueuePointer::SendAll => ControllerEffect::FlushPendingInputs,
    }
}

fn apply_plan_pointer(state: &mut TuiState, pointer: PlanPointer) {
    match pointer {
        PlanPointer::Fold(node_id) => {
            state.plan_mut().focus();
            state.plan_mut().toggle_collapse(&node_id);
        }
        PlanPointer::Row(node_id) => {
            let inspect =
                state.plan().is_focused() && state.plan().selected_node_id() == Some(&node_id);
            state.plan_mut().focus();
            if inspect {
                state.plan_mut().open_inspector();
            } else {
                state.plan_mut().select_node(node_id);
            }
        }
    }
}

fn accept_completion_at(
    state: &mut TuiState,
    index: usize,
    visible_rows: usize,
) -> ControllerEffect {
    if state.history_search_active() {
        let _ = state.accept_history_search_visible(index, visible_rows);
        return ControllerEffect::None;
    }
    let slash = state
        .completion_menu()
        .is_some_and(crate::tui::completion::CompletionMenu::is_slash);
    if !state.select_completion(index) {
        return ControllerEffect::None;
    }
    state.accept_completion();
    if slash && input_is_known_slash(state) {
        handle_key_action(KeyAction::Submit, state)
    } else {
        ControllerEffect::None
    }
}

pub(crate) fn handle_mouse_input(
    mouse: MouseInput,
    terminal_size: Size,
    state: &mut TuiState,
) -> ControllerEffect {
    if state.overlay().is_some() {
        state.clear_text_selection();
        state.set_composer_mouse_selecting(false);
        return match mouse {
            MouseInput::Down(position) => match pointer_target(state, terminal_size, position) {
                PointerTarget::Overlay(pointer) => overlay_pointer_effect(pointer, state),
                _ => ControllerEffect::None,
            },
            MouseInput::Drag(_) | MouseInput::Up(_) => ControllerEffect::None,
        };
    }
    if let MouseInput::Drag(position) = mouse
        && state.is_composer_mouse_selecting()
    {
        let rects = cockpit_rects(terminal_size, state);
        position_composer_cursor(state, rects.input, terminal_size.height, position, true);
        return ControllerEffect::None;
    }
    if let MouseInput::Up(_) = mouse
        && state.is_composer_mouse_selecting()
    {
        state.set_composer_mouse_selecting(false);
        return ControllerEffect::None;
    }
    if let MouseInput::Drag(position) = mouse
        && state
            .text_selection()
            .is_some_and(|selection| selection.terminal_size() == terminal_size)
    {
        state.drag_text_selection(position);
        return ControllerEffect::None;
    }
    let rects = cockpit_rects(terminal_size, state);
    state.validate_text_selection_area(crate::tui::render::timeline_text_area(
        state,
        rects.timeline,
    ));
    match mouse {
        MouseInput::Down(position) => {
            state.clear_text_selection();
            state.set_composer_mouse_selecting(false);
            match pointer_target(state, terminal_size, position) {
                PointerTarget::Passthrough => {}
                PointerTarget::Consumed => return ControllerEffect::None,
                PointerTarget::Model => return open_model_from_header(state),
                PointerTarget::Status(action) => return status_click(action, state),
                PointerTarget::Composer => {
                    let effect = if state.is_editing_pending() {
                        commit_pending_edit_effect(state, true)
                    } else {
                        ControllerEffect::None
                    };
                    state.plan_mut().leave_focus();
                    if position_composer_cursor(
                        state,
                        rects.input,
                        terminal_size.height,
                        position,
                        false,
                    ) {
                        state.set_composer_mouse_selecting(true);
                    }
                    return effect;
                }
                PointerTarget::PendingQueue(pointer) => {
                    return apply_pending_queue_pointer(state, pointer);
                }
                PointerTarget::Plan(pointer) => {
                    apply_plan_pointer(state, pointer);
                    return ControllerEffect::None;
                }
                PointerTarget::Completion(index) => {
                    return accept_completion_at(
                        state,
                        index,
                        usize::from(rects.completion.height).max(1),
                    );
                }
                PointerTarget::Overlay(pointer) => return overlay_pointer_effect(pointer, state),
            }
            match crate::tui::render::timeline_mouse_down(
                state,
                rects.timeline,
                terminal_size,
                position,
            ) {
                crate::tui::render::TimelineMouseDown::Copy(text) => {
                    return ControllerEffect::CopyText(text);
                }
                crate::tui::render::TimelineMouseDown::CopyTooLarge => {
                    return ControllerEffect::CopyTextTooLarge;
                }
                crate::tui::render::TimelineMouseDown::ToggleCommand(index) => {
                    state.toggle_command_expanded(index);
                    return ControllerEffect::None;
                }
                crate::tui::render::TimelineMouseDown::ToggleThinking(index) => {
                    state.toggle_thinking_expanded(index);
                    return ControllerEffect::None;
                }
                crate::tui::render::TimelineMouseDown::InspectSubagent(agent_id) => {
                    state.open_subagent_inspect(Some(&agent_id));
                    return ControllerEffect::RefreshSubagents;
                }
                crate::tui::render::TimelineMouseDown::TaskDock(dock_pointer) => {
                    use crate::tui::render::task_dock::TaskDockPointer;
                    match dock_pointer {
                        TaskDockPointer::ToggleExpanded => {
                            state.task_dock_mut().toggle_expanded();
                            return ControllerEffect::None;
                        }
                        TaskDockPointer::SelectFilter(filter) => {
                            state.task_dock_mut().set_filter(filter);
                            return ControllerEffect::None;
                        }
                        TaskDockPointer::InspectBackgroundProcess(process_id) => {
                            state.open_background_task_inspect(process_id.clone());
                            return ControllerEffect::InspectBackgroundProcess(process_id);
                        }
                        TaskDockPointer::StopBackgroundProcess(process_id) => {
                            return ControllerEffect::StopBackgroundProcess(process_id);
                        }
                        TaskDockPointer::InspectSubagent(agent_id) => {
                            state.open_subagent_inspect(Some(&agent_id));
                            return ControllerEffect::RefreshSubagents;
                        }
                        TaskDockPointer::CloseSubagent(agent_id) => {
                            state.close_subagent_from_dock(&agent_id);
                            return ControllerEffect::None;
                        }
                    }
                }
                crate::tui::render::TimelineMouseDown::Select(selection) => {
                    state.begin_text_selection(selection);
                }
                crate::tui::render::TimelineMouseDown::None => {}
            }
            ControllerEffect::None
        }
        MouseInput::Drag(position) => {
            state.drag_text_selection(position);
            ControllerEffect::None
        }
        MouseInput::Up(position) => {
            let had_selection = state.text_selection().is_some();
            match state.finish_text_selection(position) {
                Ok(Some(captured)) if state.is_commenting() => {
                    let occurrence = crate::tui::render::quote_occurrence(
                        state,
                        rects.timeline,
                        captured.item_index,
                        &captured.text,
                        captured.visual_row,
                        captured.column,
                    );
                    state.capture_comment_selection(captured.text, captured.item_index, occurrence);
                    ControllerEffect::None
                }
                Ok(Some(captured)) => ControllerEffect::CopyText(captured.text),
                Ok(None) => {
                    if let Some(hit) =
                        crate::tui::render::comment_mark_at(state, rects.timeline, position)
                        && (had_selection || hit.in_sticky_header)
                    {
                        state.edit_comment(hit.index);
                    }
                    ControllerEffect::None
                }
                Err(CopyTextError::TooLarge) => ControllerEffect::CopyTextTooLarge,
            }
        }
    }
}

pub(crate) fn handle_mouse_scroll_up(
    position: Position,
    terminal_size: Size,
    state: &mut TuiState,
) -> ControllerEffect {
    handle_mouse_scroll_up_by(position, terminal_size, state.mouse_scroll_lines(), state)
}

pub(crate) fn handle_mouse_scroll_down(
    position: Position,
    terminal_size: Size,
    state: &mut TuiState,
) -> ControllerEffect {
    handle_mouse_scroll_down_by(position, terminal_size, state.mouse_scroll_lines(), state)
}

pub(crate) fn handle_mouse_scroll_up_by(
    position: Position,
    terminal_size: Size,
    lines: usize,
    state: &mut TuiState,
) -> ControllerEffect {
    let lines = lines.max(1);
    state.clear_text_selection();
    if matches!(state.overlay(), Some(Overlay::CommandDetails(_))) {
        let area = Rect::new(0, 0, terminal_size.width, terminal_size.height);
        return crate::tui::command_details::handle_mouse_scroll(state, area, position, true);
    }
    if scroll_active_overlay(state, true, lines) {
        return ControllerEffect::None;
    }
    if state.overlay().is_some() {
        return ControllerEffect::None;
    }
    if state.history_search_active() {
        state.newer_history_search();
        return ControllerEffect::None;
    }
    if scroll_task_dock_at(position, terminal_size, state, -1) {
        return ControllerEffect::None;
    }
    if position_in_plan_pane(position, terminal_size, state) {
        let plan_lines = lines.min(PLAN_SCROLL_STEP);
        if state.plan().is_inspector_open() {
            state.plan_mut().scroll_inspector_up_by(plan_lines);
        } else {
            state.plan_mut().scroll_up_by(plan_lines);
        }
    } else {
        state.scroll_timeline_up_by(lines);
    }
    ControllerEffect::None
}

pub(crate) fn handle_mouse_scroll_down_by(
    position: Position,
    terminal_size: Size,
    lines: usize,
    state: &mut TuiState,
) -> ControllerEffect {
    let lines = lines.max(1);
    state.clear_text_selection();
    if matches!(state.overlay(), Some(Overlay::CommandDetails(_))) {
        let area = Rect::new(0, 0, terminal_size.width, terminal_size.height);
        return crate::tui::command_details::handle_mouse_scroll(state, area, position, false);
    }
    if scroll_active_overlay(state, false, lines) {
        return ControllerEffect::None;
    }
    if state.overlay().is_some() {
        return ControllerEffect::None;
    }
    if state.history_search_active() {
        state.begin_or_older_history_search();
        return ControllerEffect::None;
    }
    if scroll_task_dock_at(position, terminal_size, state, 1) {
        return ControllerEffect::None;
    }
    if position_in_plan_pane(position, terminal_size, state) {
        let plan_lines = lines.min(PLAN_SCROLL_STEP);
        if state.plan().is_inspector_open() {
            state.plan_mut().scroll_inspector_down_by(plan_lines);
        } else {
            state.plan_mut().scroll_down_by(plan_lines);
        }
    } else {
        state.scroll_timeline_down_by(lines);
    }
    ControllerEffect::None
}

fn scroll_task_dock_at(
    position: Position,
    terminal_size: Size,
    state: &mut TuiState,
    delta: isize,
) -> bool {
    if !state.task_dock().is_expanded() {
        return false;
    }
    let rects = cockpit_rects(terminal_size, state);
    let dock_rect = crate::tui::render::timeline_task_dock_rect(state, rects.timeline);
    if !dock_rect.contains(position) {
        return false;
    }
    let visible_rows = usize::from(dock_rect.height.saturating_sub(2))
        .clamp(1, crate::tui::task_dock::MAX_DOCK_VISIBLE_ROWS);
    let total_items = state.task_dock().entries(state.subagents()).len();
    state
        .task_dock_mut()
        .scroll_by(delta, total_items, visible_rows);
    true
}

pub(super) fn position_in_plan_pane(
    position: Position,
    terminal_size: Size,
    state: &TuiState,
) -> bool {
    cockpit_rects(terminal_size, state)
        .plan
        .is_some_and(|plan| plan.contains(position))
}

fn position_composer_cursor(
    state: &mut TuiState,
    input_region: Rect,
    terminal_height: u16,
    position: Position,
    extend_selection: bool,
) -> bool {
    let text_rect =
        crate::tui::render::composer_text_rect_for_region(input_region, terminal_height);
    if text_rect.is_empty() {
        return false;
    }
    if !extend_selection && (position.y < text_rect.y || position.y >= text_rect.bottom()) {
        return false;
    }
    let clamped_y = position
        .y
        .clamp(text_rect.y, text_rect.bottom().saturating_sub(1));
    let rel_row = usize::from(clamped_y.saturating_sub(text_rect.y));
    let rel_col = if position.x < text_rect.x {
        0
    } else if position.x >= text_rect.right() {
        usize::from(text_rect.width)
    } else {
        usize::from(position.x.saturating_sub(text_rect.x))
    };
    state.click_input_at(
        usize::from(text_rect.width),
        usize::from(text_rect.height),
        rel_row,
        rel_col,
        extend_selection,
    );
    true
}
