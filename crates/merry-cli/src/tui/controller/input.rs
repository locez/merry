//! Synchronous input-to-effect translation and local UI state updates; no external I/O.

use crate::tui::{
    click_target::{OverlayPointer, PointerTarget, StatusAction, pointer_target},
    command_details::CommandNavigation,
    controller::{ControllerEffect, cockpit_rects, exit_review_if_active},
    copy_controls::CopyTextError,
    input::{DraftImage, TuiSubmission},
    keymap::KeyAction,
    overlay::{Overlay, OverlayKeyResult},
    plan_render::PlanPointer,
    preferences::TuiPreferences,
    provider_overlay::ModelPickerTarget,
    state::TuiState,
    text_interaction::MouseInput,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::QueuedInputLane;
use ratatui::layout::{Position, Size};

pub(super) const TIMELINE_SCROLL_STEP: usize = 5;

pub(super) const PLAN_SCROLL_STEP: usize = 5;

pub(crate) fn handle_key_action(action: KeyAction, state: &mut TuiState) -> ControllerEffect {
    match action {
        KeyAction::SubmitNext => dispatch_submit(state, ControllerEffect::SubmitNext),
        KeyAction::SubmitBacklog => dispatch_submit(state, ControllerEffect::SubmitBacklog),
        KeyAction::CancelInputOrQuit => {
            if state.discard_comment_draft() {
                ControllerEffect::None
            } else if state.cancel_input_or_mark_quit() {
                ControllerEffect::Quit
            } else {
                ControllerEffect::None
            }
        }
        KeyAction::InsertNewline => {
            state.insert_input_newline();
            ControllerEffect::None
        }
        KeyAction::PasteImage => ControllerEffect::PasteImage,
        #[cfg(feature = "web")]
        KeyAction::OpenSessionInBrowser => ControllerEffect::OpenSessionInBrowser,
        KeyAction::OpenCommandPanel => {
            state.open_command_palette();
            ControllerEffect::None
        }
        KeyAction::OpenCommandDetails => {
            if matches!(state.overlay(), Some(Overlay::CommandDetails(_))) {
                state.close_overlay();
                ControllerEffect::None
            } else {
                crate::tui::command_details::inspect_command(state, CommandNavigation::Latest)
            }
        }
        KeyAction::TogglePlan => {
            state.plan_mut().toggle();
            ControllerEffect::None
        }
        KeyAction::Interrupt => {
            if state.can_interrupt_run() {
                ControllerEffect::Interrupt
            } else if state.is_interrupting() {
                state.follow_latest();
                state.plan_mut().leave_focus();
                state.repeat_stop_feedback();
                ControllerEffect::None
            } else if state.is_timeline_reviewing() {
                state.exit_timeline_review();
                ControllerEffect::None
            } else {
                state.escape_comment_mode();
                ControllerEffect::None
            }
        }
        KeyAction::Quit => ControllerEffect::Quit,
        KeyAction::ScrollUp => {
            state.scroll_timeline_up_by(TIMELINE_SCROLL_STEP);
            ControllerEffect::None
        }
        KeyAction::ScrollDown => {
            state.scroll_timeline_down_by(TIMELINE_SCROLL_STEP);
            ControllerEffect::None
        }
        KeyAction::FollowLatest => {
            state.follow_latest();
            ControllerEffect::None
        }
        KeyAction::ReviewPreviousUserInput => {
            state.jump_to_previous_user_input();
            ControllerEffect::None
        }
        KeyAction::HistoryPrevious => {
            state.previous_input_history();
            ControllerEffect::None
        }
        KeyAction::HistoryNext => {
            state.next_input_history();
            ControllerEffect::None
        }
        KeyAction::HistorySearch => {
            state.begin_or_older_history_search();
            ControllerEffect::None
        }
        KeyAction::ResumeSuspended => ControllerEffect::ResumeSuspended,
        KeyAction::DiscardSuspended => ControllerEffect::DiscardSuspended,
        KeyAction::ToggleComment => {
            state.toggle_comment_mode();
            ControllerEffect::None
        }
        KeyAction::SendComments => send_comments(state, ControllerEffect::SubmitNext),
        KeyAction::CloseOverlay => ControllerEffect::None,
    }
}

fn dispatch_submit(
    state: &mut TuiState,
    submit: impl FnOnce(TuiSubmission) -> ControllerEffect,
) -> ControllerEffect {
    if !state.is_commenting() {
        return submit_input(state, submit);
    }
    if let Some(effect) = crate::tui::command_controller::slash_input_effect(state) {
        exit_review_if_active(state);
        return effect;
    }
    if exit_review_if_active(state) {
        return ControllerEffect::None;
    }
    if state.pending_comment_quote().is_some() {
        state.pin_pending_comment();
        return ControllerEffect::None;
    }
    if state.comment_count() > 0 {
        return send_comments(state, submit);
    }
    submit_input(state, submit)
}

fn send_comments(
    state: &mut TuiState,
    submit: impl FnOnce(TuiSubmission) -> ControllerEffect,
) -> ControllerEffect {
    state
        .take_comment_submission()
        .map_or(ControllerEffect::None, submit)
}

pub(super) fn submit_input(
    state: &mut TuiState,
    submit: impl FnOnce(TuiSubmission) -> ControllerEffect,
) -> ControllerEffect {
    if let Some(effect) = crate::tui::command_controller::slash_input_effect(state) {
        exit_review_if_active(state);
        return effect;
    }
    if exit_review_if_active(state) {
        return ControllerEffect::None;
    }
    state
        .take_input_for_submit()
        .map_or(ControllerEffect::None, submit)
}

fn history_search_key(key: KeyEvent, state: &mut TuiState) -> ControllerEffect {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('r') if control && !alt => {
            state.begin_or_older_history_search();
            ControllerEffect::None
        }
        KeyCode::Char('s') if control && !alt => {
            state.newer_history_search();
            ControllerEffect::None
        }
        KeyCode::Char('g' | 'c') if control && !alt => {
            state.cancel_history_search();
            ControllerEffect::None
        }
        KeyCode::Esc => {
            state.cancel_history_search();
            ControllerEffect::None
        }
        KeyCode::Enter => {
            state.accept_history_search();
            ControllerEffect::None
        }
        KeyCode::Backspace if !control && !alt => {
            state.history_search_backspace();
            ControllerEffect::None
        }
        KeyCode::Char(character) if !control && !alt => {
            state.history_search_push(character);
            ControllerEffect::None
        }
        _ => {
            state.accept_history_search();
            handle_key_event(key, state)
        }
    }
}

pub(crate) fn handle_key_event(key: KeyEvent, state: &mut TuiState) -> ControllerEffect {
    if state.overlay().is_none() && state.clear_text_selection() && key.code == KeyCode::Esc {
        return ControllerEffect::None;
    }
    if state.overlay().is_none() && state.history_search_active() {
        return history_search_key(key, state);
    }
    if matches!(state.overlay(), Some(Overlay::CommandDetails(_)))
        && state.keymap().action_for(key.into()) == Some(KeyAction::OpenCommandDetails)
    {
        return handle_key_action(KeyAction::OpenCommandDetails, state);
    }
    if let Some(overlay) = state.overlay_mut() {
        let result = overlay.handle_key(key);
        return overlay_key_effect(result, state);
    }

    if state.completion_menu().is_some() {
        let slash_completion = state
            .completion_menu()
            .is_some_and(crate::tui::completion::CompletionMenu::is_slash);
        match key.code {
            KeyCode::Enter if slash_completion => {
                state.accept_completion();
                if input_is_known_slash(state) {
                    return handle_key_action(KeyAction::SubmitNext, state);
                }
                return ControllerEffect::None;
            }
            KeyCode::Enter | KeyCode::Tab => {
                state.accept_completion();
                return ControllerEffect::None;
            }
            KeyCode::Down => {
                state.select_next_completion();
                return ControllerEffect::None;
            }
            KeyCode::Up => {
                state.select_previous_completion();
                return ControllerEffect::None;
            }
            KeyCode::Esc => {
                state.close_completion_menu();
                return ControllerEffect::None;
            }
            _ => {}
        }
    }

    if state.plan().is_focused() {
        if crate::tui::plan_controller::handle_navigation_key(key, state) {
            return ControllerEffect::None;
        }
        if let Some(action) = state.keymap().action_for(key.into())
            && matches!(
                action,
                KeyAction::OpenCommandPanel
                    | KeyAction::OpenCommandDetails
                    | KeyAction::FollowLatest
                    | KeyAction::TogglePlan
                    | KeyAction::ToggleComment
                    | KeyAction::HistorySearch
                    | KeyAction::Interrupt
                    | KeyAction::Quit
            )
        {
            return handle_key_action(action, state);
        }
        return ControllerEffect::None;
    }

    if key.code == KeyCode::Backspace && key.modifiers.is_empty() && state.restore_last_comment() {
        return ControllerEffect::None;
    }

    if key.modifiers.is_empty()
        && matches!(key.code, KeyCode::Up | KeyCode::Down)
        && state.move_input_line(key.code == KeyCode::Down)
    {
        return ControllerEffect::None;
    }

    if let Some(action) = state.keymap().action_for(key.into()) {
        return handle_key_action(action, state);
    }
    state.handle_input_key(key);
    ControllerEffect::None
}

pub(super) fn preferences_effect(
    item: crate::tui::overlay::SettingItem,
    preferences: TuiPreferences,
) -> ControllerEffect {
    match item {
        crate::tui::overlay::SettingItem::CodeTheme => {
            ControllerEffect::PersistPreferences(preferences)
        }
        crate::tui::overlay::SettingItem::KeyboardShortcuts => ControllerEffect::None,
        _ => ControllerEffect::ApplyRuntimePreferences(preferences),
    }
}

pub(super) fn input_is_known_slash(state: &TuiState) -> bool {
    state.plain_input_text().is_some_and(|text| {
        matches!(
            crate::tui::command::match_slash_input(text),
            crate::tui::command::SlashCommandMatch::Known(_)
        )
    })
}

fn overlay_key_effect(result: OverlayKeyResult, state: &mut TuiState) -> ControllerEffect {
    match result {
        OverlayKeyResult::Consumed => ControllerEffect::None,
        OverlayKeyResult::PreviousCommand => {
            crate::tui::command_details::inspect_command(state, CommandNavigation::Previous)
        }
        OverlayKeyResult::NextCommand => {
            crate::tui::command_details::inspect_command(state, CommandNavigation::Next)
        }
        OverlayKeyResult::CopyText(text) => ControllerEffect::CopyText(text),
        OverlayKeyResult::Close => {
            state.close_overlay();
            ControllerEffect::None
        }
        OverlayKeyResult::Back => {
            state.back_overlay();
            ControllerEffect::None
        }
        OverlayKeyResult::Run(command) => {
            crate::tui::command_controller::run_palette_command(command, state)
        }
        OverlayKeyResult::AdjustSetting(item, direction) => {
            if state.adjust_setting(item, direction) {
                preferences_effect(item, state.preferences().clone())
            } else {
                ControllerEffect::None
            }
        }
        OverlayKeyResult::ResetSetting(item) => {
            if state.reset_setting(item) {
                preferences_effect(item, state.preferences().clone())
            } else {
                ControllerEffect::None
            }
        }
        OverlayKeyResult::BeginModelEdit => {
            state.begin_settings_model_edit();
            ControllerEffect::None
        }
        OverlayKeyResult::CommitModel(value) => {
            let clears_model = value.trim().is_empty();
            match state.commit_settings_model(value) {
                Some((alias, model)) => ControllerEffect::OpenReasoningPicker {
                    alias,
                    model,
                    target: ModelPickerTarget::ActiveProvider,
                },
                None if clears_model => {
                    ControllerEffect::ApplyRuntimePreferences(state.preferences().clone())
                }
                None => ControllerEffect::None,
            }
        }
        OverlayKeyResult::BeginReasoningEdit => {
            state.begin_settings_reasoning_edit();
            ControllerEffect::None
        }
        OverlayKeyResult::CommitReasoning(value) => {
            if state.commit_settings_reasoning(value) {
                ControllerEffect::ApplyRuntimePreferences(state.preferences().clone())
            } else {
                ControllerEffect::None
            }
        }
        OverlayKeyResult::BeginContextWindowEdit => {
            state.begin_settings_context_window_edit();
            ControllerEffect::None
        }
        OverlayKeyResult::CommitContextWindow(value) => {
            if state.commit_settings_context_window(value) {
                ControllerEffect::ApplyRuntimePreferences(state.preferences().clone())
            } else {
                ControllerEffect::None
            }
        }
        OverlayKeyResult::OpenShortcuts => {
            state.open_shortcuts();
            ControllerEffect::None
        }
        OverlayKeyResult::ConfirmPlanApproval => {
            let Some(input) = state.plan_approval_input() else {
                state.close_overlay();
                return ControllerEffect::None;
            };
            state.close_overlay();
            ControllerEffect::ApprovePlan(input)
        }
        OverlayKeyResult::ApprovePermission(approval_id) => {
            state.close_overlay();
            ControllerEffect::ApprovePermission(approval_id)
        }
        OverlayKeyResult::DenyPermission(approval_id) => {
            state.close_overlay();
            ControllerEffect::DenyPermission(approval_id)
        }
        OverlayKeyResult::Provider(action) => {
            crate::tui::controller_provider::provider_overlay_effect(action)
        }
    }
}

fn press_overlay(state: &mut TuiState, code: KeyCode) -> ControllerEffect {
    let Some(overlay) = state.overlay_mut() else {
        return ControllerEffect::None;
    };
    let result = overlay.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    overlay_key_effect(result, state)
}

fn overlay_pointer_effect(pointer: OverlayPointer, state: &mut TuiState) -> ControllerEffect {
    match pointer {
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
        OverlayPointer::Provider(index) => {
            if let Some(Overlay::ProviderManager(manager)) = state.overlay_mut() {
                manager.select(index);
            }
            press_overlay(state, KeyCode::Enter)
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
    let key_action = match action {
        StatusAction::Commands => KeyAction::OpenCommandPanel,
        StatusAction::Plan => KeyAction::TogglePlan,
        StatusAction::Follow => KeyAction::FollowLatest,
        StatusAction::Stop => KeyAction::Interrupt,
        StatusAction::Comment => KeyAction::ToggleComment,
        StatusAction::SendComments => KeyAction::SendComments,
    };
    handle_key_action(key_action, state)
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

fn accept_completion_at(state: &mut TuiState, index: usize) -> ControllerEffect {
    let slash = state
        .completion_menu()
        .is_some_and(crate::tui::completion::CompletionMenu::is_slash);
    if !state.select_completion(index) {
        return ControllerEffect::None;
    }
    state.accept_completion();
    if slash && input_is_known_slash(state) {
        handle_key_action(KeyAction::SubmitNext, state)
    } else {
        ControllerEffect::None
    }
}

pub(crate) fn handle_paste_event(text: &str, state: &mut TuiState) {
    state.clear_text_selection();
    if !state.insert_overlay_paste(text) {
        state.insert_input_paste(text);
    }
}

pub(crate) fn handle_mouse_input(
    mouse: MouseInput,
    terminal_size: Size,
    state: &mut TuiState,
) -> ControllerEffect {
    if state.overlay().is_some() {
        state.clear_text_selection();
        return match mouse {
            MouseInput::Down(position) => match pointer_target(state, terminal_size, position) {
                PointerTarget::Overlay(pointer) => overlay_pointer_effect(pointer, state),
                _ => ControllerEffect::None,
            },
            MouseInput::Drag(_) | MouseInput::Up(_) => ControllerEffect::None,
        };
    }
    let rects = cockpit_rects(terminal_size, state);
    state.validate_text_selection_area(crate::tui::render::timeline_text_area(
        state,
        rects.timeline,
    ));
    match mouse {
        MouseInput::Down(position) => {
            state.clear_text_selection();
            match pointer_target(state, terminal_size, position) {
                PointerTarget::Passthrough => {}
                PointerTarget::Consumed => return ControllerEffect::None,
                PointerTarget::Model => return open_model_from_header(state),
                PointerTarget::Status(action) => return status_click(action, state),
                PointerTarget::Composer => {
                    state.plan_mut().leave_focus();
                    return ControllerEffect::None;
                }
                PointerTarget::Plan(pointer) => {
                    apply_plan_pointer(state, pointer);
                    return ControllerEffect::None;
                }
                PointerTarget::Completion(index) => return accept_completion_at(state, index),
                PointerTarget::Overlay(pointer) => return overlay_pointer_effect(pointer, state),
            }
            match crate::tui::render::timeline_mouse_down(state, rects.timeline, position) {
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
) {
    state.clear_text_selection();
    if let Some(crate::tui::overlay::Overlay::CommandDetails(details)) = state.overlay_mut() {
        details.handle_key(KeyEvent::new(
            KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE,
        ));
        return;
    }
    if state.overlay().is_some() {
        return;
    }
    if position_in_plan_pane(position, terminal_size, state) {
        if state.plan().is_inspector_open() {
            state.plan_mut().scroll_inspector_up_by(PLAN_SCROLL_STEP);
        } else {
            state.plan_mut().scroll_up_by(PLAN_SCROLL_STEP);
        }
    } else {
        state.scroll_timeline_up_by(TIMELINE_SCROLL_STEP);
    }
}

pub(crate) fn handle_mouse_scroll_down(
    position: Position,
    terminal_size: Size,
    state: &mut TuiState,
) {
    state.clear_text_selection();
    if let Some(crate::tui::overlay::Overlay::CommandDetails(details)) = state.overlay_mut() {
        details.handle_key(KeyEvent::new(
            KeyCode::PageDown,
            crossterm::event::KeyModifiers::NONE,
        ));
        return;
    }
    if state.overlay().is_some() {
        return;
    }
    if position_in_plan_pane(position, terminal_size, state) {
        if state.plan().is_inspector_open() {
            state.plan_mut().scroll_inspector_down_by(PLAN_SCROLL_STEP);
        } else {
            state.plan_mut().scroll_down_by(PLAN_SCROLL_STEP);
        }
    } else {
        state.scroll_timeline_down_by(TIMELINE_SCROLL_STEP);
    }
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

pub(in crate::tui) fn project_local_effect(effect: &ControllerEffect, state: &mut TuiState) {
    match effect {
        ControllerEffect::SubmitNext(submission) => {
            state.push_local_user_echo(submission.text.clone(), QueuedInputLane::Next);
            state.project_local_run_start();
        }
        ControllerEffect::SubmitBacklog(submission) => {
            state.push_local_user_echo(submission.text.clone(), QueuedInputLane::Backlog);
            state.project_local_run_start();
        }
        ControllerEffect::Interrupt => {
            state.follow_latest();
            state.plan_mut().leave_focus();
            state.begin_stop_feedback();
        }
        ControllerEffect::PersistPreferences(_)
        | ControllerEffect::ApplyRuntimePreferences(_)
        | ControllerEffect::OpenProviderManager
        | ControllerEffect::OpenProviderForm
        | ControllerEffect::OpenProviderEditor(_)
        | ControllerEffect::OpenModelPicker(_)
        | ControllerEffect::SaveProvider(_)
        | ControllerEffect::UpdateProvider { .. }
        | ControllerEffect::RefreshModels(_)
        | ControllerEffect::DeleteProvider(_)
        | ControllerEffect::SelectProvider { .. }
        | ControllerEffect::OpenReasoningPicker { .. }
        | ControllerEffect::ApplyProviderModel { .. } => {}
        _ => {}
    }
}

pub(crate) fn apply_clipboard_image_completion(
    result: Result<DraftImage, String>,
    state: &mut TuiState,
) {
    let result = result.and_then(|image| {
        state
            .insert_input_image(image)
            .map_err(|error| error.to_string())
    });
    if let Err(error) = result {
        state.push_timeline_item(crate::tui::state::TimelineItem::Diagnostic {
            title: "clipboard_image".to_owned(),
            body: error,
        });
    }
}
