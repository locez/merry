use super::{
    input::{DraftImage, TuiSubmission},
    input_history_store::InputHistoryStore,
    keymap::KeyAction,
    layout::{BottomPaneHeights, cockpit_layout},
    overlay::OverlayKeyResult,
    preferences::{TuiPreferences, TuiPreferencesStore},
    projector::TuiProjector,
    provider_overlay::{ModelPickerTarget, ProviderFormValues},
    render,
    runtime::TuiRuntimeSession,
    state::{TimelineItem, TuiState},
    terminal::{TerminalEvent, TerminalSession},
};
use crate::{
    cli_error::{CliError, unexpected},
    provider_management::ProviderManagementService,
    web::{RuntimeWebService, open_in_browser},
};
use crossterm::event::{KeyCode, KeyEvent};
use futures_util::StreamExt;
use merry_core::QueuedInputLane;
use merry_runtime::InterruptReason;
use ratatui::layout::{Position, Rect, Size};
use std::time::Duration;
use tokio::{sync::mpsc, time};
use tokio_util::sync::CancellationToken;

const TIMELINE_SCROLL_STEP: usize = 5;
const PLAN_SCROLL_STEP: usize = 5;
const TUI_REFRESH_INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct ModelDiscoveryCompletion {
    pub(super) generation: u64,
    pub(super) alias: String,
    pub(super) result: Result<Vec<super::provider_overlay::ModelListItem>, String>,
}

struct ClipboardImageCompletion {
    result: Result<DraftImage, String>,
}

pub(super) struct ProviderController<'a> {
    pub(super) management: &'a mut ProviderManagementService,
    pub(super) discovery_tx: &'a mpsc::Sender<ModelDiscoveryCompletion>,
    pub(super) discovery_generation: &'a mut u64,
    pub(super) discovery_token: &'a mut Option<CancellationToken>,
}

pub(super) use super::controller_provider::provider_discovery_draft;

struct InputHistoryController<'a> {
    store: &'a InputHistoryStore,
    warning_shown: &'a mut bool,
}

struct ControllerServices<'a> {
    preferences_store: &'a TuiPreferencesStore,
    input_history: InputHistoryController<'a>,
    providers: ProviderController<'a>,
    clipboard_image_tx: &'a mpsc::Sender<ClipboardImageCompletion>,
    web_service: &'a mut RuntimeWebService,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerEffect {
    None,
    SubmitNext(TuiSubmission),
    SubmitBacklog(TuiSubmission),
    PasteImage,
    Interrupt,
    ResumeSuspended,
    DiscardSuspended,
    PersistPreferences(TuiPreferences),
    ApplyRuntimePreferences(TuiPreferences),
    OpenSessionInBrowser,
    OpenProviderManager,
    OpenProviderForm,
    OpenProviderEditor(String),
    OpenModelPicker(String),
    BackToProviderForm,
    DiscoverFormModels {
        original_alias: Option<String>,
        values: ProviderFormValues,
    },
    SaveProvider(ProviderFormValues),
    UpdateProvider {
        original_alias: String,
        values: ProviderFormValues,
    },
    RefreshModels(String),
    RefreshFormModels,
    DeleteProvider(String),
    SelectProvider {
        alias: String,
    },
    OpenReasoningPicker {
        alias: String,
        model: String,
        target: ModelPickerTarget,
    },
    ApplyProviderModel {
        alias: String,
        model: String,
        reasoning_effort: merry_llm::ReasoningEffort,
        target: ModelPickerTarget,
    },
    EnterPlanMode,
    ApprovePlan(merry_runtime::PlanApprovalInput),
    ApprovePermission(String),
    DenyPermission(String),
    RevisePlan,
    RetryPlanNode(merry_core::PlanNodeId),
    CancelPlan,
    SaveSession,
    Quit,
}

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

fn submit_input(
    state: &mut TuiState,
    submit: impl FnOnce(TuiSubmission) -> ControllerEffect,
) -> ControllerEffect {
    if let Some(effect) = super::command_controller::slash_input_effect(state) {
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

fn exit_review_if_active(state: &mut TuiState) -> bool {
    let was_reviewing = state.is_timeline_reviewing();
    if state.is_timeline_reviewing() {
        state.exit_timeline_review();
    }
    was_reviewing
}

pub(crate) fn handle_key_event(key: KeyEvent, state: &mut TuiState) -> ControllerEffect {
    if state.overlay().is_some() {
        let result = state
            .overlay_mut()
            .expect("overlay checked above")
            .handle_key(key);
        return match result {
            OverlayKeyResult::Consumed => ControllerEffect::None,
            OverlayKeyResult::Close => {
                state.close_overlay();
                ControllerEffect::None
            }
            OverlayKeyResult::Back => {
                state.back_overlay();
                ControllerEffect::None
            }
            OverlayKeyResult::Run(command) => {
                super::command_controller::run_palette_command(command, state)
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
                super::controller_provider::provider_overlay_effect(action)
            }
        };
    }

    if state.completion_menu().is_some() {
        let slash_completion = state
            .completion_menu()
            .is_some_and(super::completion::CompletionMenu::is_slash);
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
        if super::plan_controller::handle_navigation_key(key, state) {
            return ControllerEffect::None;
        }
        if let Some(action) = state.keymap().action_for(key.into())
            && matches!(
                action,
                KeyAction::OpenCommandPanel
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

fn preferences_effect(
    item: super::overlay::SettingItem,
    preferences: TuiPreferences,
) -> ControllerEffect {
    match item {
        super::overlay::SettingItem::CodeTheme => ControllerEffect::PersistPreferences(preferences),
        super::overlay::SettingItem::KeyboardShortcuts => ControllerEffect::None,
        _ => ControllerEffect::ApplyRuntimePreferences(preferences),
    }
}

fn input_is_known_slash(state: &TuiState) -> bool {
    state.plain_input_text().is_some_and(|text| {
        matches!(
            super::command::match_slash_input(text),
            super::command::SlashCommandMatch::Known(_)
        )
    })
}

pub(crate) fn handle_paste_event(text: &str, state: &mut TuiState) {
    if !state.insert_overlay_paste(text) {
        state.insert_input_paste(text);
    }
}

pub(crate) fn handle_mouse_scroll_up(
    position: Position,
    terminal_size: Size,
    state: &mut TuiState,
) {
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

fn position_in_plan_pane(position: Position, terminal_size: Size, state: &TuiState) -> bool {
    cockpit_rects(terminal_size, state)
        .plan
        .is_some_and(|plan| plan.contains(position))
}

fn cockpit_rects(terminal_size: Size, state: &TuiState) -> super::layout::TimelineRects {
    let area = Rect::new(0, 0, terminal_size.width, terminal_size.height);
    let pane_heights = render::pane_heights_for_area(state, area);
    cockpit_layout(
        area,
        BottomPaneHeights {
            queue: pane_heights.queue,
            completion: pane_heights.completion,
            input: pane_heights.input,
            status: render::STATUS_HEIGHT,
        },
        state.plan().is_open(),
        state.plan().is_focused(),
    )
}

pub(crate) async fn run_controller(
    mut terminal: TerminalSession,
    mut session: TuiRuntimeSession,
    mut state: TuiState,
    preferences_store: TuiPreferencesStore,
    input_history_store: InputHistoryStore,
    mut provider_management: ProviderManagementService,
) -> Result<(), CliError> {
    let mut projector = TuiProjector::default();
    let (model_discovery_tx, mut model_discovery_rx) = mpsc::channel(4);
    let (clipboard_image_tx, mut clipboard_image_rx) = mpsc::channel(4);
    let mut model_discovery_generation = 0_u64;
    let mut model_discovery_token: Option<CancellationToken> = None;
    let mut subagent_activity_open = true;
    let mut permission_requests_open = true;
    let mut input_history_warning_shown = false;
    let mut web_service = RuntimeWebService::new(session.runtime().clone());
    if let Err(error) = web_service.start().await {
        state.push_timeline_item(TimelineItem::Diagnostic {
            title: "Web service unavailable".to_owned(),
            body: error.to_string(),
        });
    }
    state
        .plan_mut()
        .update_subagent_activity(session.subagent_activity.borrow().clone());
    let mut refresh_interval = new_refresh_interval();
    refresh_interval.tick().await;
    render_once(&mut terminal, &state)?;

    loop {
        tokio::select! {
            _ = refresh_interval.tick(), if state.is_active_run() => {
                if let Some(next) = session.prune_cancelled_permission_reviews() {
                    match next {
                        Some((approval_id, body)) => state.open_permission_review(approval_id, body),
                        None => state.close_overlay(),
                    }
                }
                render_once(&mut terminal, &state)?;
            }
            event = terminal.next_event() => {
                let Some(event) = event.map_err(unexpected)? else {
                    break;
                };

                match event {
                    TerminalEvent::Key(key) => {
                        let effect = handle_key_event(key, &mut state);
                        project_local_effect(&effect, &mut state);
                        render_once(&mut terminal, &state)?;
                        let providers = ProviderController {
                            management: &mut provider_management,
                            discovery_tx: &model_discovery_tx,
                            discovery_generation: &mut model_discovery_generation,
                            discovery_token: &mut model_discovery_token,
                        };
                        let input_history = InputHistoryController {
                            store: &input_history_store,
                            warning_shown: &mut input_history_warning_shown,
                        };
                        let services = ControllerServices {
                            preferences_store: &preferences_store,
                            input_history,
                            providers,
                            clipboard_image_tx: &clipboard_image_tx,
                            web_service: &mut web_service,
                        };
                        let should_quit = dispatch_effect(
                            effect,
                            &mut session,
                            &mut state,
                            services,
                        )
                        .await?;
                        if should_quit {
                            break;
                        }
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::MouseScrollUp(position) => {
                        let size = terminal.size().map_err(unexpected)?;
                        handle_mouse_scroll_up(position, size, &mut state);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::MouseScrollDown(position) => {
                        let size = terminal.size().map_err(unexpected)?;
                        handle_mouse_scroll_down(position, size, &mut state);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::Paste(text) => {
                        handle_paste_event(&text, &mut state);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::Resize => {
                        render_once(&mut terminal, &state)?;
                    }
                }
            }
            event = session.stream.next() => {
                let Some(event) = event else {
                    break;
                };
                projector.apply(event, &mut state);
                render_once(&mut terminal, &state)?;
            }
            activity = session.subagent_activity.changed(), if subagent_activity_open => {
                match activity {
                    Ok(()) => {
                        state.plan_mut().update_subagent_activity(
                            session.subagent_activity.borrow().clone(),
                        );
                        render_once(&mut terminal, &state)?;
                    }
                    Err(_) => {
                        subagent_activity_open = false;
                    }
                }
            }
            request = session.permission_requests.recv(), if permission_requests_open => {
                let Some(request) = request else {
                    permission_requests_open = false;
                    continue;
                };
                if let Some((approval_id, body)) = session.enqueue_permission_review(request) {
                    state.open_permission_review(approval_id, body);
                }
                render_once(&mut terminal, &state)?;
            }
            completion = model_discovery_rx.recv() => {
                let Some(completion) = completion else {
                    continue;
                };
                if completion.generation == model_discovery_generation {
                    state.update_model_picker(&completion.alias, completion.result);
                    render_once(&mut terminal, &state)?;
                }
            }
            completion = clipboard_image_rx.recv() => {
                let Some(completion) = completion else {
                    continue;
                };
                apply_clipboard_image_completion(completion.result, &mut state);
                render_once(&mut terminal, &state)?;
            }
        }
    }

    web_service.shutdown().await.map_err(unexpected)?;

    Ok(())
}

async fn dispatch_effect(
    effect: ControllerEffect,
    session: &mut TuiRuntimeSession,
    state: &mut TuiState,
    services: ControllerServices<'_>,
) -> Result<bool, CliError> {
    let ControllerServices {
        preferences_store,
        input_history,
        providers,
        clipboard_image_tx,
        web_service,
    } = services;
    if let Some(should_quit) =
        super::plan_controller::dispatch_effect(&effect, session, state).await
    {
        return Ok(should_quit);
    }
    if super::controller_provider::is_provider_effect(&effect) {
        return super::controller_provider::dispatch_provider_effect(
            effect,
            session,
            state,
            preferences_store,
            providers,
        )
        .await;
    }
    match effect {
        ControllerEffect::None => Ok(false),
        ControllerEffect::SubmitNext(submission) => {
            let (message, history_text) = submission
                .into_user_message_and_history()
                .map_err(unexpected)?;
            session
                .input
                .submit_next_message(message)
                .await
                .map_err(unexpected)?;
            persist_submitted_input_history(
                input_history.store,
                state,
                &history_text,
                input_history.warning_shown,
            )
            .await;
            Ok(false)
        }
        ControllerEffect::SubmitBacklog(submission) => {
            let (message, history_text) = submission
                .into_user_message_and_history()
                .map_err(unexpected)?;
            session
                .input
                .enqueue_message(message)
                .await
                .map_err(unexpected)?;
            persist_submitted_input_history(
                input_history.store,
                state,
                &history_text,
                input_history.warning_shown,
            )
            .await;
            Ok(false)
        }
        ControllerEffect::PasteImage => {
            start_clipboard_image_read(clipboard_image_tx.clone());
            Ok(false)
        }
        ControllerEffect::OpenSessionInBrowser => {
            let url = match web_service.session_url(&session.metadata.session_id).await {
                Ok(url) => url,
                Err(error) => {
                    state.push_timeline_item(TimelineItem::Diagnostic {
                        title: "Web service unavailable".to_owned(),
                        body: error.to_string(),
                    });
                    return Ok(false);
                }
            };
            match open_in_browser(&url).await {
                Ok(()) => state.push_timeline_item(TimelineItem::LocalCommand {
                    title: "Trajectory opened".to_owned(),
                    body: url,
                }),
                Err(error) => state.push_timeline_item(TimelineItem::Diagnostic {
                    title: "Could not open browser".to_owned(),
                    body: format!("{error}. Open this URL manually: {url}"),
                }),
            }
            Ok(false)
        }
        ControllerEffect::ApprovePermission(approval_id) => {
            if let Some(next) = session.resolve_permission_review(&approval_id, true)? {
                state.open_permission_review(next.0, next.1);
            }
            Ok(false)
        }
        ControllerEffect::DenyPermission(approval_id) => {
            if let Some(next) = session.resolve_permission_review(&approval_id, false)? {
                state.open_permission_review(next.0, next.1);
            }
            Ok(false)
        }
        ControllerEffect::Interrupt => {
            let control = session.control.clone();
            let _interrupt_task = tokio::spawn(async move {
                if let Err(error) = control.interrupt(InterruptReason::User).await {
                    tracing::warn!(error = %error, "interactive interrupt request failed");
                }
            });
            Ok(false)
        }
        ControllerEffect::ResumeSuspended => {
            session
                .control
                .resume_suspended()
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::DiscardSuspended => {
            session
                .control
                .discard_suspended()
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::SaveSession => {
            session.set_title(state.latest_user_input_title());
            match session.save_now().await {
                Ok(()) => state.push_timeline_item(TimelineItem::LocalCommand {
                    title: "Session saved".to_owned(),
                    body: session.metadata.session_id.as_str().to_owned(),
                }),
                Err(error) => {
                    tracing::warn!(error = ?error, "explicit TUI session save failed");
                    state.push_timeline_item(TimelineItem::Diagnostic {
                        title: "Session save failed".to_owned(),
                        body: "Session state could not be written. The TUI is still open; check the logs and retry at an idle boundary."
                            .to_owned(),
                    });
                }
            }
            Ok(false)
        }
        ControllerEffect::PersistPreferences(preferences) => {
            preferences_store
                .save(&preferences)
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::ApplyRuntimePreferences(preferences) => {
            session.apply_preferences(&preferences).await?;
            preferences_store
                .save(&preferences)
                .await
                .map_err(unexpected)?;
            state.set_model_label(session.model_label.clone());
            state.set_reasoning_effort_label(session.reasoning_effort_label.clone());
            Ok(false)
        }
        ControllerEffect::EnterPlanMode
        | ControllerEffect::ApprovePlan(_)
        | ControllerEffect::RevisePlan
        | ControllerEffect::RetryPlanNode(_)
        | ControllerEffect::CancelPlan => unreachable!("plan effect handled above"),
        ControllerEffect::Quit => {
            session.set_title(state.latest_user_input_title());
            session.control.close().await.map_err(unexpected)?;
            session.stream.wait_until_closed().await;
            session.save_on_exit().await?;
            Ok(true)
        }
        _ => unreachable!("provider effect handled by the provider dispatcher"),
    }
}

pub(super) async fn persist_submitted_input_history(
    store: &InputHistoryStore,
    state: &mut TuiState,
    text: &str,
    warning_shown: &mut bool,
) {
    if text.trim().is_empty() {
        return;
    }
    state.record_input_history(text);
    match store.record(text).await {
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(error = %error, "could not persist accepted TUI input history");
            if !*warning_shown {
                state.push_timeline_item(TimelineItem::Diagnostic {
                    title: "Input history not saved".to_owned(),
                    body: "The message was accepted, but shared input history could not be written. In-memory history remains available for this session."
                        .to_owned(),
                });
                *warning_shown = true;
            }
        }
    }
}

pub(super) fn project_local_effect(effect: &ControllerEffect, state: &mut TuiState) {
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

#[cfg(target_os = "linux")]
fn start_clipboard_image_read(sender: mpsc::Sender<ClipboardImageCompletion>) {
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(|| {
            super::clipboard_image::read_clipboard_image()
                .map_err(|error| error.to_string())
                .and_then(|image| image.into_draft_image().map_err(|error| error.to_string()))
        })
        .await
        .unwrap_or_else(|error| Err(format!("clipboard image task failed: {error}")));
        let _ = sender.send(ClipboardImageCompletion { result }).await;
    });
}

#[cfg(not(target_os = "linux"))]
fn start_clipboard_image_read(sender: mpsc::Sender<ClipboardImageCompletion>) {
    tokio::spawn(async move {
        let _ =
            sender
                .send(ClipboardImageCompletion {
                    result: Err(
                        "clipboard image paste is currently supported only on Linux".to_owned()
                    ),
                })
                .await;
    });
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
        state.push_timeline_item(super::state::TimelineItem::Diagnostic {
            title: "clipboard_image".to_owned(),
            body: error,
        });
    }
}

fn render_once(terminal: &mut TerminalSession, state: &TuiState) -> Result<(), CliError> {
    terminal
        .draw(|frame| render::render(frame, state))
        .map_err(unexpected)?;
    Ok(())
}

fn new_refresh_interval() -> time::Interval {
    let mut interval = time::interval(TUI_REFRESH_INTERVAL);
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    interval
}

#[cfg(test)]
mod tests {
    use super::{TUI_REFRESH_INTERVAL, new_refresh_interval};
    use std::time::Duration;

    #[tokio::test]
    async fn refresh_interval_is_not_reset_by_unrelated_work() {
        let mut refresh_interval = new_refresh_interval();
        refresh_interval.tick().await;

        for _ in 0..8 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let tick = tokio::time::timeout(TUI_REFRESH_INTERVAL, refresh_interval.tick()).await;
        assert!(
            tick.is_ok(),
            "the persistent ticker should remain scheduled"
        );
    }
}
