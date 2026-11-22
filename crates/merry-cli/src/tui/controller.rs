//! TUI event-loop and background-task ownership; input translation and effects are separate.

pub(super) use super::controller_provider::provider_discovery_draft;
#[cfg(feature = "web")]
use crate::web::RuntimeWebService;
use crate::{
    cli_error::{CliError, unexpected},
    provider_management::ProviderManagementService,
    tui::{
        controller::effects::{ControllerServices, InputHistoryController, dispatch_effect},
        input::{DraftImage, TuiSubmission},
        input_history_store::InputHistoryStore,
        layout::{BottomPaneHeights, cockpit_layout},
        preferences::{TuiPreferences, TuiPreferencesStore},
        projector::TuiProjector,
        provider_overlay::{ModelPickerTarget, ProviderFormValues},
        render,
        runtime::TuiRuntimeSession,
        state::TuiState,
        terminal::{TerminalEvent, TerminalSession},
    },
};
#[cfg(test)]
pub(super) use effects::persist_submitted_input_history;
pub(super) use input::project_local_effect;
pub(crate) use input::{
    apply_clipboard_image_completion, handle_key_action, handle_key_event, handle_paste_event,
};
use merry_runtime::InteractiveRunMessage;
pub(crate) use pointer::{
    handle_mouse_input, handle_mouse_scroll_down, handle_mouse_scroll_down_by,
    handle_mouse_scroll_up, handle_mouse_scroll_up_by,
};
use ratatui::layout::{Position, Rect, Size};
use std::time::Duration;
use tokio::{sync::mpsc, task::JoinSet, time};
use tokio_util::sync::CancellationToken;

mod effects;
mod input;
mod pointer;

const TUI_REFRESH_INTERVAL: Duration = crate::tui::motion::ACTIVE_MOTION_INTERVAL;
const MAX_DRAINED_TERMINAL_EVENTS: usize = 64;
/// Terminals such as Ghostty default to `mouse-scroll-multiplier = 3`, sending
/// 3-4 wheel sequences per physical detent. Bursts up to this size map to one
/// configured notch so a single wheel click does not jump 15-20 lines.
const WHEEL_BURST_NOTCH_EVENTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CoalescedTerminalEvent {
    Event(TerminalEvent),
    MouseScroll {
        position: Position,
        up: bool,
        lines: usize,
    },
}

/// Coalesces already-queued wheel bursts, mouse drags, and resize events within
/// one event-loop batch while preserving ordering around keys, clicks, and pastes.
pub(super) fn coalesce_terminal_events(
    events: Vec<TerminalEvent>,
    wheel_lines: usize,
) -> Vec<CoalescedTerminalEvent> {
    let wheel_lines = wheel_lines.max(1);
    let mut out: Vec<CoalescedTerminalEvent> = Vec::with_capacity(events.len());
    let mut index = 0;

    while index < events.len() {
        match &events[index] {
            TerminalEvent::MouseScrollUp(_) | TerminalEvent::MouseScrollDown(_) => {
                let mut net: isize = 0;
                let mut last_position = Position::default();
                while index < events.len() {
                    match events[index] {
                        TerminalEvent::MouseScrollUp(position) => {
                            net = net.saturating_add(1);
                            last_position = position;
                            index += 1;
                        }
                        TerminalEvent::MouseScrollDown(position) => {
                            net = net.saturating_sub(1);
                            last_position = position;
                            index += 1;
                        }
                        _ => break,
                    }
                }
                let magnitude = net.unsigned_abs();
                if magnitude > 0 {
                    let lines = if magnitude <= WHEEL_BURST_NOTCH_EVENTS {
                        wheel_lines
                    } else {
                        wheel_lines
                            .saturating_add(magnitude - WHEEL_BURST_NOTCH_EVENTS)
                            .min(wheel_lines.saturating_mul(4))
                    };
                    out.push(CoalescedTerminalEvent::MouseScroll {
                        position: last_position,
                        up: net > 0,
                        lines,
                    });
                }
            }
            TerminalEvent::Mouse(crate::tui::text_interaction::MouseInput::Drag(_)) => {
                let mut latest = events[index].clone();
                index += 1;
                while index < events.len()
                    && matches!(
                        events[index],
                        TerminalEvent::Mouse(crate::tui::text_interaction::MouseInput::Drag(_))
                    )
                {
                    latest = events[index].clone();
                    index += 1;
                }
                out.push(CoalescedTerminalEvent::Event(latest));
            }
            TerminalEvent::Resize => {
                index += 1;
                while index < events.len() && matches!(events[index], TerminalEvent::Resize) {
                    index += 1;
                }
                out.push(CoalescedTerminalEvent::Event(TerminalEvent::Resize));
            }
            _ => {
                out.push(CoalescedTerminalEvent::Event(events[index].clone()));
                index += 1;
            }
        }
    }

    out
}

fn needs_pre_dispatch_render(effect: &ControllerEffect) -> bool {
    matches!(
        effect,
        ControllerEffect::Submit(_)
            | ControllerEffect::Interrupt
            | ControllerEffect::LoadCommandOutput(_)
            | ControllerEffect::PasteImage
            | ControllerEffect::OpenModelPicker(_)
            | ControllerEffect::RefreshModels(_)
            | ControllerEffect::RefreshFormModels
            | ControllerEffect::DiscoverFormModels { .. }
    )
}

pub(super) struct ModelDiscoveryCompletion {
    pub(super) generation: u64,
    pub(super) alias: String,
    pub(super) result: Result<Vec<super::provider_overlay::ModelListItem>, String>,
}

struct ClipboardImageCompletion {
    result: Result<DraftImage, String>,
}

pub(super) struct CommandOutputCompletion {
    call_id: merry_core::ToolCallId,
    generation: u64,
    result: Result<super::command_details::CapturedOutput, String>,
}

impl CommandOutputCompletion {
    pub(super) fn new(
        call_id: merry_core::ToolCallId,
        generation: u64,
        result: Result<super::command_details::CapturedOutput, String>,
    ) -> Self {
        Self {
            call_id,
            generation,
            result,
        }
    }

    pub(super) fn apply(self, state: &mut TuiState) {
        if self.generation == state.command_details_generation() {
            super::command_details::apply_output(state, &self.call_id, self.result);
        }
    }
}

pub(super) struct ProviderController<'a> {
    pub(super) management: &'a mut ProviderManagementService,
    pub(super) discovery_tx: &'a mpsc::Sender<ModelDiscoveryCompletion>,
    pub(super) discovery_generation: &'a mut u64,
    pub(super) discovery_token: &'a mut Option<CancellationToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerEffect {
    None,
    Submit(TuiSubmission),
    SetPendingPaused(bool),
    UpdatePendingInput {
        id: u64,
        text: String,
        resume: bool,
    },
    RemovePendingInput {
        id: u64,
        resume: Option<bool>,
    },
    FlushPendingInputs,
    PasteImage,
    LoadCommandOutput(merry_core::ToolCallId),
    CopyText(String),
    CopyTextTooLarge,
    Interrupt,
    PersistPreferences(TuiPreferences),
    ApplyRuntimePreferences(TuiPreferences),
    #[cfg(feature = "web")]
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
    SetPlanMode {
        enabled: bool,
        initial_prompt: Option<String>,
    },
    ApprovePlan {
        plan_id: merry_core::PlanId,
        note: Option<String>,
    },
    RequestPlanChanges {
        plan_id: merry_core::PlanId,
        feedback: String,
    },
    AbandonPlan(merry_core::PlanId),
    ApprovePermission(String),
    DenyPermission(String),
    SaveSession,
    ShowTasks,
    RefreshSubagents,
    SendSubagentMessage {
        agent_id: String,
        message: String,
    },
    CancelSubagent(String),
    InspectBackgroundProcess(String),
    StopBackgroundProcess(String),
    Quit,
}

fn exit_review_if_active(state: &mut TuiState) -> bool {
    let was_reviewing = state.is_timeline_reviewing();
    if state.is_timeline_reviewing() {
        state.exit_timeline_review();
    }
    was_reviewing
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
    let mut background_tasks = JoinSet::new();
    let (model_discovery_tx, mut model_discovery_rx) = mpsc::channel(4);
    let (clipboard_image_tx, mut clipboard_image_rx) = mpsc::channel(4);
    let (command_output_tx, mut command_output_rx) = mpsc::channel(4);
    let mut model_discovery_generation = 0_u64;
    let mut model_discovery_token: Option<CancellationToken> = None;
    let mut subagent_activity_open = true;
    let mut permission_requests_open = true;
    let mut input_history_warning_shown = false;
    #[cfg(feature = "web")]
    let mut web_service = RuntimeWebService::new(session.runtime().clone());
    let mut bg_poll_ticks: u8 = 0;
    let mut subagent_live_stream: Option<(
        String,
        tokio::sync::broadcast::Receiver<merry_core::RuntimeEvent>,
    )> = None;
    let run_result: Result<(), CliError> = async {
        state.update_subagent_activity(&session.subagent_activity.borrow().clone());
        state.sync_background_processes(session.runtime().background_processes());
        let mut refresh_interval = new_refresh_interval();
        refresh_interval.tick().await;
        render_once(&mut terminal, &mut state)?;

        loop {
            tokio::select! {
                Some(result) = background_tasks.join_next(), if !background_tasks.is_empty() => {
                    result.map_err(unexpected)?;
                }
                _ = refresh_interval.tick(), if needs_refresh(&state) => {
                    bg_poll_ticks = bg_poll_ticks.wrapping_add(1);
                    if bg_poll_ticks.is_multiple_of(30)
                        && (state.has_running_background_processes()
                            || state.inspected_background_process_id().is_some())
                    {
                        state.sync_background_processes(session.runtime().background_processes());
                        if let Some(process_id) = state.inspected_background_process_id()
                            && let Ok(id) = merry_runtime::BackgroundProcessId::new(process_id.clone())
                        {
                            match session.runtime().read_background_process(&id) {
                                Ok(output) => state.update_background_task_output(output),
                                Err(err) => {
                                    state.update_background_task_error(&process_id, err.message())
                                }
                            }
                        }
                    }
                    if let Some(next) = session.prune_cancelled_permission_reviews() {
                        match next {
                            Some(view) => state.open_permission_review_view(view),
                            None => state.close_overlay(),
                        }
                    }
                    refresh_interactions(&mut state, terminal.size().map_err(unexpected)?);
                    render_once(&mut terminal, &mut state)?;
                }
                event = terminal.next_event() => {
                    let Some(first_event) = event.map_err(unexpected)? else {
                        break;
                    };
                    let mut raw_batch = vec![first_event];
                    let extra = terminal
                        .drain_queued_events(MAX_DRAINED_TERMINAL_EVENTS.saturating_sub(1))
                        .map_err(unexpected)?;
                    raw_batch.extend(extra);

                    let coalesced = coalesce_terminal_events(raw_batch, state.mouse_scroll_lines());
                    let mut should_exit = false;
                    for item in coalesced {
                        let effect = match item {
                            CoalescedTerminalEvent::Event(TerminalEvent::Key(key)) => {
                                Some(handle_key_event(key, &mut state))
                            }
                            CoalescedTerminalEvent::Event(TerminalEvent::Mouse(mouse)) => {
                                let size = terminal.size().map_err(unexpected)?;
                                Some(handle_mouse_input(mouse, size, &mut state))
                            }
                            CoalescedTerminalEvent::MouseScroll { position, up, lines } => {
                                let size = terminal.size().map_err(unexpected)?;
                                let effect = if up {
                                    handle_mouse_scroll_up_by(position, size, lines, &mut state)
                                } else {
                                    handle_mouse_scroll_down_by(position, size, lines, &mut state)
                                };
                                (effect != ControllerEffect::None).then_some(effect)
                            }
                            CoalescedTerminalEvent::Event(TerminalEvent::MouseScrollUp(position)) => {
                                let size = terminal.size().map_err(unexpected)?;
                                let effect = handle_mouse_scroll_up(position, size, &mut state);
                                (effect != ControllerEffect::None).then_some(effect)
                            }
                            CoalescedTerminalEvent::Event(TerminalEvent::MouseScrollDown(position)) => {
                                let size = terminal.size().map_err(unexpected)?;
                                let effect = handle_mouse_scroll_down(position, size, &mut state);
                                (effect != ControllerEffect::None).then_some(effect)
                            }
                            CoalescedTerminalEvent::Event(TerminalEvent::Paste(text)) => {
                                handle_paste_event(&text, &mut state);
                                None
                            }
                            CoalescedTerminalEvent::Event(TerminalEvent::Resize) => {
                                state.clear_text_selection();
                                None
                            }
                        };
                        if let Some(effect) = effect
                            && effect != ControllerEffect::None
                        {
                            project_local_effect(&effect, &mut state);
                            if needs_pre_dispatch_render(&effect) {
                                render_once(&mut terminal, &mut state)?;
                            }
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
                                command_output_tx: &command_output_tx,
                                terminal: &mut terminal,
                                #[cfg(feature = "web")]
                                web_service: &mut web_service,
                                background_tasks: &mut background_tasks,
                                subagent_live_stream: &mut subagent_live_stream,
                            };
                            if dispatch_effect(effect, &mut session, &mut state, services).await? {
                                should_exit = true;
                                break;
                            }
                        }
                    }
                    if state.inspected_subagent_id().is_none() {
                        subagent_live_stream = None;
                    }
                    if should_exit {
                        break;
                    }
                    render_once(&mut terminal, &mut state)?;
                }
                subagent_event = recv_subagent_live_event(&mut subagent_live_stream), if subagent_live_stream.is_some() => {
                    match subagent_event {
                        Some((agent_id, event)) => {
                            let stream_progress = matches!(
                                event,
                                merry_core::RuntimeEvent::AssistantMessageDelta { .. }
                                    | merry_core::RuntimeEvent::ReasoningDelta { .. }
                                    | merry_core::RuntimeEvent::ToolCallOutputDelta { .. }
                                    | merry_core::RuntimeEvent::ModelOutputRateUpdated { .. }
                            );
                            state.apply_subagent_live_event(&agent_id, event);
                            if !stream_progress || !needs_refresh(&state) {
                                render_once(&mut terminal, &mut state)?;
                            }
                        }
                        None => {
                            subagent_live_stream = None;
                        }
                    }
                }
                message = session.stream.next_message() => {
                    let Some(message) = message.map_err(unexpected)? else {
                        break;
                    };
                    match message {
                        InteractiveRunMessage::Event(event) => {
                            let was_active = state.is_active_run();
                            let stream_progress = matches!(event,
                                merry_core::RuntimeEvent::AssistantMessageDelta { .. }
                                | merry_core::RuntimeEvent::ReasoningDelta { .. }
                                | merry_core::RuntimeEvent::ToolCallOutputDelta { .. }
                                | merry_core::RuntimeEvent::ModelOutputRateUpdated { .. });
                            let tool_finished = matches!(
                                event,
                                merry_core::RuntimeEvent::ToolCallFinished { .. }
                            );
                            projector.apply(event, &mut state);
                            if tool_finished {
                                state.sync_background_processes(
                                    session.runtime().background_processes(),
                                );
                            }
                            if was_active && !state.is_active_run() {
                                session
                                    .sync_metadata_on_turn_settled(state.first_user_input_topic())
                                    .await;
                            }
                            if !stream_progress || !state.is_active_run() {
                                render_once(&mut terminal, &mut state)?;
                            }
                        }
                        InteractiveRunMessage::ToolInvocations { batch } => {
                            return Err(unexpected(format!(
                                "Rust TUI received {} host tool invocations, but its coding profile requires runtime-owned tools",
                                batch.calls().len()
                            )));
                        }
                        _ => {
                            return Err(unexpected(
                                "Rust TUI received an unsupported interactive run message",
                            ));
                        }
                    }
                }
                activity = session.subagent_activity.changed(), if subagent_activity_open => {
                    match activity {
                        Ok(()) => {
                            let snapshots = session.subagent_activity.borrow().clone();
                            state.update_subagent_activity(&snapshots);
                            render_once(&mut terminal, &mut state)?;
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
                    if let Some(view) = session.enqueue_permission_review(request) {
                        state.open_permission_review_view(view);
                    } else {
                        state.set_permission_review_queue_total(
                            session.pending_permission_review_count(),
                        );
                    }
                    render_once(&mut terminal, &mut state)?;
                }
                completion = model_discovery_rx.recv() => {
                    let Some(completion) = completion else {
                        continue;
                    };
                    if completion.generation == model_discovery_generation {
                        state.update_model_picker(&completion.alias, completion.result);
                        render_once(&mut terminal, &mut state)?;
                    }
                }
                completion = clipboard_image_rx.recv() => {
                    let Some(completion) = completion else {
                        continue;
                    };
                    apply_clipboard_image_completion(completion.result, &mut state);
                    render_once(&mut terminal, &mut state)?;
                }
                Some(completion) = command_output_rx.recv() => {
                    completion.apply(&mut state);
                    render_once(&mut terminal, &mut state)?;
                }
            }
        }

        Ok(())
    }.await;

    session.stream.request_cancel();
    if let Some(token) = model_discovery_token.take() {
        token.cancel();
    }
    model_discovery_rx.close();
    clipboard_image_rx.close();
    command_output_rx.close();
    let session_result = session.stream.wait_until_closed().await.map_err(unexpected);
    let tasks_result = drain_background_tasks(&mut background_tasks).await;
    #[cfg(feature = "web")]
    let web_result = web_service.shutdown().await.map_err(unexpected);
    #[cfg(not(feature = "web"))]
    let web_result = Ok(());
    let mut cleanup_result = Ok(());
    for result in [session_result, tasks_result, web_result] {
        if let Err(error) = result {
            tracing::warn!(error = ?error, "TUI shutdown failed");
            if cleanup_result.is_ok() {
                cleanup_result = Err(error);
            }
        }
    }
    run_result.and(cleanup_result)
}

async fn drain_background_tasks(tasks: &mut JoinSet<()>) -> Result<(), CliError> {
    let mut first_error = None;
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            tracing::warn!(error = %error, "TUI background task failed");
            first_error.get_or_insert_with(|| unexpected(error));
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn render_once(terminal: &mut TerminalSession, state: &mut TuiState) -> Result<(), CliError> {
    render::prepare_viewport(state, terminal.size().map_err(unexpected)?);
    terminal
        .draw(|frame| render::render(frame, state))
        .map_err(unexpected)?;
    Ok(())
}

fn needs_refresh(state: &TuiState) -> bool {
    state.is_active_run()
        || state.has_active_subagents()
        || state.has_running_background_processes()
        || state.inspected_background_process_id().is_some()
        || state.has_active_timeline_scroll_animation()
        || state.clipboard_feedback().is_some()
        || state.text_selection_is_autoscrolling()
}

async fn recv_subagent_live_event(
    stream: &mut Option<(
        String,
        tokio::sync::broadcast::Receiver<merry_core::RuntimeEvent>,
    )>,
) -> Option<(String, merry_core::RuntimeEvent)> {
    let (agent_id, receiver) = stream.as_mut()?;
    loop {
        match receiver.recv().await {
            Ok(event) => return Some((agent_id.clone(), event)),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
        }
    }
}

fn refresh_interactions(state: &mut TuiState, size: Size) {
    state.expire_clipboard_feedback();
    let rects = cockpit_rects(size, state);
    state.validate_text_selection_area(render::timeline_text_area(state, rects.timeline));
    state.autoscroll_text_selection();
}

fn new_refresh_interval() -> time::Interval {
    let mut interval = time::interval(TUI_REFRESH_INTERVAL);
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    interval
}

#[cfg(test)]
mod tests;
