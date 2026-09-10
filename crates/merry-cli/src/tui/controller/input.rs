//! Synchronous input-to-effect translation and local UI state updates; no external I/O.

use crate::tui::{
    command_details::CommandNavigation,
    controller::{ControllerEffect, cockpit_rects, exit_review_if_active},
    copy_controls::CopyTextError,
    input::{DraftImage, TuiSubmission},
    keymap::KeyAction,
    overlay::{Overlay, OverlayKeyResult},
    preferences::TuiPreferences,
    provider_overlay::ModelPickerTarget,
    state::TuiState,
    text_interaction::MouseInput,
};
use crossterm::event::{KeyCode, KeyEvent};
use merry_core::QueuedInputLane;
use ratatui::layout::{Position, Size};

pub(super) const TIMELINE_SCROLL_STEP: usize = 5;

pub(super) const PLAN_SCROLL_STEP: usize = 5;

pub(crate) fn handle_key_action(action: KeyAction, state: &mut TuiState) -> ControllerEffect {
    match action {
        KeyAction::SubmitNext => submit_input(state, ControllerEffect::SubmitNext),
        KeyAction::SubmitBacklog => submit_input(state, ControllerEffect::SubmitBacklog),
        KeyAction::CancelInputOrQuit => {
            if state.cancel_input_or_mark_quit() {
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
        KeyAction::ResumeSuspended => ControllerEffect::ResumeSuspended,
        KeyAction::DiscardSuspended => ControllerEffect::DiscardSuspended,
        _ => ControllerEffect::None,
    }
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

pub(crate) fn handle_key_event(key: KeyEvent, state: &mut TuiState) -> ControllerEffect {
    if state.overlay().is_none() && state.clear_text_selection() && key.code == KeyCode::Esc {
        return ControllerEffect::None;
    }
    if matches!(state.overlay(), Some(Overlay::CommandDetails(_)))
        && state.keymap().action_for(key.into()) == Some(KeyAction::OpenCommandDetails)
    {
        return handle_key_action(KeyAction::OpenCommandDetails, state);
    }
    if let Some(overlay) = state.overlay_mut() {
        let result = overlay.handle_key(key);
        return match result {
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
        };
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
                    | KeyAction::Interrupt
                    | KeyAction::Quit
            )
        {
            return handle_key_action(action, state);
        }
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
        return ControllerEffect::None;
    }
    let rects = cockpit_rects(terminal_size, state);
    state.validate_text_selection_area(crate::tui::render::timeline_content_region(rects.timeline));
    match mouse {
        MouseInput::Down(position) => {
            state.clear_text_selection();
            match crate::tui::render::timeline_mouse_down(state, rects.timeline, position) {
                crate::tui::render::TimelineMouseDown::Copy(text) => {
                    return ControllerEffect::CopyText(text);
                }
                crate::tui::render::TimelineMouseDown::CopyTooLarge => {
                    return ControllerEffect::CopyTextTooLarge;
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
        MouseInput::Up(position) => match state.finish_text_selection(position) {
            Ok(Some(text)) => ControllerEffect::CopyText(text),
            Ok(None) => ControllerEffect::None,
            Err(CopyTextError::TooLarge) => ControllerEffect::CopyTextTooLarge,
        },
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
