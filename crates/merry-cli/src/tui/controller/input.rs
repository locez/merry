//! Synchronous keyboard and paste input-to-effect translation; pointer routing lives in `pointer.rs`.

use crate::tui::{
    command_details::CommandNavigation,
    controller::{ControllerEffect, exit_review_if_active},
    input::{DraftImage, TuiSubmission},
    keymap::KeyAction,
    overlay::{Overlay, OverlayKeyResult},
    preferences::TuiPreferences,
    provider_overlay::ModelPickerTarget,
    state::{PendingEditCommit, TuiState},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub(super) const TIMELINE_SCROLL_STEP: usize = 5;

pub(super) const PLAN_SCROLL_STEP: usize = 5;

pub(crate) fn handle_key_action(action: KeyAction, state: &mut TuiState) -> ControllerEffect {
    match action {
        KeyAction::Submit => dispatch_submit(state, ControllerEffect::Submit),
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
        KeyAction::ToggleComment => {
            state.toggle_comment_mode();
            ControllerEffect::None
        }
        KeyAction::SendComments => send_comments(state, ControllerEffect::Submit),
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
    if let Some(submission) = state.take_input_for_submit() {
        return submit(submission);
    }
    if !state.is_active_run() && state.queue_preview().paused && state.has_queue_preview_items() {
        return ControllerEffect::FlushPendingInputs;
    }
    ControllerEffect::None
}

fn history_search_key(key: KeyEvent, state: &mut TuiState) -> ControllerEffect {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let super_key = key.modifiers.contains(KeyModifiers::SUPER);
    match key.code {
        KeyCode::Char('r' | 'R') if control && !alt && !super_key => {
            state.begin_or_older_history_search();
            ControllerEffect::None
        }
        KeyCode::Char('s' | 'S') if control && !alt && !super_key => {
            state.newer_history_search();
            ControllerEffect::None
        }
        KeyCode::Down if !control && !alt && !shift && !super_key => {
            state.begin_or_older_history_search();
            ControllerEffect::None
        }
        KeyCode::Up if !control && !alt && !shift && !super_key => {
            state.newer_history_search();
            ControllerEffect::None
        }
        KeyCode::Char('c' | 'C') if (control || super_key) && !alt => {
            if let Some(selected) = state.history_search_selected_text() {
                ControllerEffect::CopyText(selected)
            } else if super_key {
                ControllerEffect::None
            } else {
                state.cancel_history_search();
                ControllerEffect::None
            }
        }
        KeyCode::Char('x' | 'X') if (control || super_key) && !alt => {
            if let Some(cut) = state.history_search_cut_selection() {
                ControllerEffect::CopyText(cut)
            } else {
                ControllerEffect::None
            }
        }
        KeyCode::Char('g' | 'G') if control && !alt && !super_key => {
            state.cancel_history_search();
            ControllerEffect::None
        }
        KeyCode::Esc => {
            if !state.history_search_clear_selection() {
                state.cancel_history_search();
            }
            ControllerEffect::None
        }
        KeyCode::Enter | KeyCode::Tab if !control && !alt && !shift && !super_key => {
            state.accept_history_search();
            ControllerEffect::None
        }
        _ => {
            state.history_search_handle_key(key);
            ControllerEffect::None
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
    if state.overlay().is_none() && state.is_editing_pending() {
        return pending_edit_key(key, state);
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
                    return handle_key_action(KeyAction::Submit, state);
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

    // Composer text-selection & modern editor shortcuts take precedence over
    // global cancel/interrupt when a selection is active in the composer.
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let super_key = key.modifiers.contains(KeyModifiers::SUPER);

    if key.code == KeyCode::Esc && key.modifiers.is_empty() && state.clear_input_selection() {
        return ControllerEffect::None;
    }

    if matches!(key.code, KeyCode::Char('c' | 'C')) && (ctrl || super_key) && !alt {
        let mapped = state.keymap().action_for(key.into());
        let allow_copy = super_key || matches!(mapped, None | Some(KeyAction::CancelInputOrQuit));
        if allow_copy && let Some(selected) = state.selected_input_text() {
            return ControllerEffect::CopyText(selected);
        }
        if super_key {
            return ControllerEffect::None;
        }
    }

    if matches!(key.code, KeyCode::Char('x' | 'X')) && (ctrl || super_key) && !alt {
        let mapped = state.keymap().action_for(key.into());
        if (super_key || mapped.is_none())
            && let Some(cut) = state.cut_input_selection()
        {
            return ControllerEffect::CopyText(cut);
        }
        if super_key {
            return ControllerEffect::None;
        }
    }

    if key.code == KeyCode::Enter && shift && !ctrl && !alt && !super_key {
        state.insert_input_newline();
        return ControllerEffect::None;
    }

    if matches!(key.code, KeyCode::Up | KeyCode::Down) && shift && !ctrl && !alt && !super_key {
        state.select_input_line(key.code == KeyCode::Down);
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
        OverlayKeyResult::SubagentPrevious => {
            state.step_subagent_inspect(-1);
            ControllerEffect::RefreshSubagents
        }
        OverlayKeyResult::SubagentNext => {
            state.step_subagent_inspect(1);
            ControllerEffect::RefreshSubagents
        }
        OverlayKeyResult::SubagentRefresh => ControllerEffect::RefreshSubagents,
        OverlayKeyResult::SubagentStop(agent_id) => ControllerEffect::CancelSubagent(agent_id),
        OverlayKeyResult::SubagentSendMessage { agent_id, message } => {
            ControllerEffect::SendSubagentMessage { agent_id, message }
        }
        OverlayKeyResult::RefreshBackgroundTask(id) => {
            ControllerEffect::InspectBackgroundProcess(id)
        }
        OverlayKeyResult::StopBackgroundTask(id) => ControllerEffect::StopBackgroundProcess(id),
    }
}

pub(super) fn press_overlay(state: &mut TuiState, code: KeyCode) -> ControllerEffect {
    let Some(overlay) = state.overlay_mut() else {
        return ControllerEffect::None;
    };
    let result = overlay.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    overlay_key_effect(result, state)
}

pub(crate) fn handle_paste_event(text: &str, state: &mut TuiState) {
    state.clear_text_selection();
    if state.overlay().is_none() && state.history_search_paste(text) {
        return;
    }
    if state.overlay().is_none() && state.paste_into_pending_edit(text) {
        return;
    }
    if !state.insert_overlay_paste(text) {
        state.insert_input_paste(text);
    }
}

pub(super) fn commit_pending_edit_effect(state: &mut TuiState, resume: bool) -> ControllerEffect {
    match state.commit_pending_edit() {
        Some(PendingEditCommit::Update { id, text }) => {
            ControllerEffect::UpdatePendingInput { id, text, resume }
        }
        Some(PendingEditCommit::Remove { id }) => ControllerEffect::RemovePendingInput {
            id,
            resume: Some(resume),
        },
        None => ControllerEffect::None,
    }
}

fn pending_edit_key(key: KeyEvent, state: &mut TuiState) -> ControllerEffect {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let super_key = key.modifiers.contains(KeyModifiers::SUPER);
    match key.code {
        KeyCode::Enter if !ctrl && !alt && !shift && !super_key => {
            commit_pending_edit_effect(state, true)
        }
        KeyCode::Esc if key.modifiers.is_empty() => {
            if state.clear_pending_edit_selection() {
                ControllerEffect::None
            } else if let Some(paused_before_edit) = state.cancel_pending_edit() {
                ControllerEffect::SetPendingPaused(paused_before_edit)
            } else {
                ControllerEffect::None
            }
        }
        KeyCode::Char('c' | 'C') if (ctrl || super_key) && !alt => {
            if let Some(selected) = state.selected_pending_edit_text() {
                ControllerEffect::CopyText(selected)
            } else if super_key {
                ControllerEffect::None
            } else if let Some(paused_before_edit) = state.cancel_pending_edit() {
                ControllerEffect::SetPendingPaused(paused_before_edit)
            } else {
                ControllerEffect::None
            }
        }
        KeyCode::Char('x' | 'X') if (ctrl || super_key) && !alt => {
            if let Some(cut) = state.cut_pending_edit_selection() {
                ControllerEffect::CopyText(cut)
            } else {
                ControllerEffect::None
            }
        }
        _ => {
            state.handle_pending_edit_key(key);
            ControllerEffect::None
        }
    }
}

pub(in crate::tui) fn project_local_effect(effect: &ControllerEffect, state: &mut TuiState) {
    match effect {
        ControllerEffect::Submit(submission) => {
            if !state.is_active_run() && !state.queue_preview().paused {
                state.push_local_user_echo(submission.text.clone());
                state.project_local_run_start();
            }
        }
        ControllerEffect::Interrupt => {
            state.follow_latest();
            state.plan_mut().leave_focus();
            state.begin_stop_feedback();
        }
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
