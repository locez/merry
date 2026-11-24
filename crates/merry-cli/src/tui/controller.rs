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
pub(super) use effects::write_system_clipboard;
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
            TerminalEvent::Mouse(
                mouse @ (crate::tui::text_interaction::MouseInput::Drag(_)
                | crate::tui::text_interaction::MouseInput::Move(_)),
            ) => {
                let kind = std::mem::discriminant(mouse);
                let mut latest = events[index].clone();
                index += 1;
                while let Some(TerminalEvent::Mouse(next)) = events.get(index)
                    && std::mem::discriminant(next) == kind
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
            | ControllerEffect::OpenTargetedModelPicker { .. }
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

pub(super) struct OpenUrlCompletion {
    url: String,
    result: Result<(), String>,
}

impl OpenUrlCompletion {
    pub(super) fn new(url: String, result: Result<(), String>) -> Self {
        Self { url, result }
    }

    pub(super) fn apply(self, state: &mut TuiState) {
        let (message, failed) = crate::tui::links::open_url_feedback(&self.url, self.result);
        state.show_clipboard_feedback(message, failed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerExit {
    Quit,
    NewSession,
    ResumeSession,
}

pub(super) struct CompactionCompletion {
    result: Result<Option<merry_runtime::CompactionOutcome>, merry_runtime::InteractiveError>,
}

impl CompactionCompletion {
    pub(super) fn new(
        result: Result<Option<merry_runtime::CompactionOutcome>, merry_runtime::InteractiveError>,
    ) -> Self {
        Self { result }
    }

    pub(super) fn apply(self, state: &mut TuiState) {
        match self.result {
            Ok(Some(_)) => {}
            Ok(None) => {
                state.follow_latest();
                state.push_timeline_item(crate::tui::state::TimelineItem::LocalCommand {
                    title: "Nothing to compact".to_owned(),
                    body: "Conversation history is already within the retained tail.".to_owned(),
                });
            }
            Err(merry_runtime::InteractiveError::CompactRequiresIdle) => {
                state.follow_latest();
                state.push_timeline_item(crate::tui::state::TimelineItem::LocalCommand {
                    title: "Compaction unavailable".to_owned(),
                    body: "Stop the active run before compacting context.".to_owned(),
                });
            }
            Err(_) => {
                // Runtime compaction failures and cancellations emit RunFailed /
                // RunCancelled events that TuiProjector already renders.
            }
        }
    }
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

pub(super) struct ArchiveSegmentCompletion {
    segment: u32,
    result: Result<Vec<merry_runtime::SessionTranscriptItem>, String>,
}

impl ArchiveSegmentCompletion {
    pub(super) fn new(
        segment: u32,
        result: Result<Vec<merry_runtime::SessionTranscriptItem>, String>,
    ) -> Self {
        Self { segment, result }
    }

    pub(super) fn apply(self, state: &mut TuiState, projector: &mut TuiProjector) {
        match self.result {
            Ok(transcript_items) => {
                let items =
                    TuiProjector::project_archived_transcript_items(transcript_items, state);
                let shift = state.prepend_archived_segment_items(self.segment, items);
                projector.shift_timeline_indices(shift);
            }
            Err(error) => {
                tracing::warn!(
                    segment = self.segment,
                    error = %error,
                    "failed to load archived session segment for TUI timeline"
                );
                state.fail_archive_segment_load(self.segment);
            }
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
    LoadArchiveSegment(u32),
    CopyText(String),
    CopyTextTooLarge,
    OpenUrl(String),
    Interrupt,
    PersistPreferences(TuiPreferences),
    ApplyRuntimePreferences(TuiPreferences),
    #[cfg(feature = "web")]
    OpenSessionInBrowser,
    OpenProviderManager,
    OpenProviderForm,
    OpenProviderEditor(String),
    OpenModelPicker(String),
    OpenTargetedModelPicker {
        alias: String,
        target: ModelPickerTarget,
    },
    BackToProviderForm,
    DiscoverFormModels {
        original_alias: Option<String>,
        values: ProviderFormValues,
    },
    CopyProviderApiKey,
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
    ApplyRoleModel {
        alias: String,
        model: String,
        target: crate::tui::provider_selection::ModelBindingTarget,
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
    NewSession,
    ResumeSession,
    CompactContext {
        focus: Option<String>,
    },
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
    terminal: &mut TerminalSession,
    mut session: TuiRuntimeSession,
    mut state: TuiState,
    preferences_store: &TuiPreferencesStore,
    input_history_store: &InputHistoryStore,
    provider_management: &mut ProviderManagementService,
) -> Result<ControllerExit, CliError> {
    let mut projector = TuiProjector::default();
    let mut background_tasks = JoinSet::new();
    let (model_discovery_tx, mut model_discovery_rx) = mpsc::channel(4);
    let (clipboard_image_tx, mut clipboard_image_rx) = mpsc::channel(4);
    let (command_output_tx, mut command_output_rx) = mpsc::channel(4);
    let (archive_segment_tx, mut archive_segment_rx) = mpsc::channel(4);
    let (compaction_tx, mut compaction_rx) = mpsc::channel(2);
    let (open_url_tx, mut open_url_rx) = mpsc::channel(4);
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
    let run_result: Result<ControllerExit, CliError> = async {
        state.update_subagent_activity(&session.subagent_activity.borrow().clone());
        state.sync_background_processes(session.runtime().background_processes());
        let mut refresh_interval = new_refresh_interval();
        refresh_interval.tick().await;
        render_once(terminal, &mut state)?;

        let mut exit_reason = ControllerExit::Quit;
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
                    render_once(terminal, &mut state)?;
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
                    let had_user_activity = raw_batch.iter().any(is_user_activity_event);

                    let coalesced = coalesce_terminal_events(raw_batch, state.mouse_scroll_lines());
                    let mut pending_exit: Option<ControllerExit> = None;
                    let hover_before = active_hovered_link(&state);
                    let only_mouse_moves = !coalesced.is_empty()
                        && coalesced.iter().all(|item| {
                            matches!(
                                item,
                                CoalescedTerminalEvent::Event(TerminalEvent::Mouse(
                                    crate::tui::text_interaction::MouseInput::Move(_)
                                ))
                            )
                        });
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
                                state.clear_all_hovered_links();
                                None
                            }
                        };
                        if let Some(effect) = effect
                            && effect != ControllerEffect::None
                        {
                            project_local_effect(&effect, &mut state);
                            if needs_pre_dispatch_render(&effect) {
                                render_once(terminal, &mut state)?;
                            }
                            let providers = ProviderController {
                                management: provider_management,
                                discovery_tx: &model_discovery_tx,
                                discovery_generation: &mut model_discovery_generation,
                                discovery_token: &mut model_discovery_token,
                            };
                            let input_history = InputHistoryController {
                                store: input_history_store,
                                warning_shown: &mut input_history_warning_shown,
                            };
                            let services = ControllerServices {
                                preferences_store,
                                input_history,
                                providers,
                                clipboard_image_tx: &clipboard_image_tx,
                                command_output_tx: &command_output_tx,
                                archive_segment_tx: &archive_segment_tx,
                                compaction_tx: &compaction_tx,
                                open_url_tx: &open_url_tx,
                                terminal,
                                #[cfg(feature = "web")]
                                web_service: &mut web_service,
                                background_tasks: &mut background_tasks,
                                subagent_live_stream: &mut subagent_live_stream,
                            };
                            if let Some(exit) =
                                dispatch_effect(effect, &mut session, &mut state, services).await?
                            {
                                pending_exit = Some(exit);
                                break;
                            }
                        }
                    }
                    if had_user_activity {
                        state.note_timeline_review_activity();
                    }
                    if state.inspected_subagent_id().is_none() {
                        subagent_live_stream = None;
                    }
                    if let Some(exit) = pending_exit {
                        exit_reason = exit;
                        break;
                    }
                    if !only_mouse_moves || active_hovered_link(&state) != hover_before {
                        render_once(terminal, &mut state)?;
                    }
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
                                render_once(terminal, &mut state)?;
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
                                if !session.is_metadata_written() {
                                    session
                                        .ensure_metadata_written(state.first_user_input_topic())
                                        .await;
                                }
                            }
                            if was_active && !state.is_active_run() {
                                session
                                    .sync_metadata_on_turn_settled(state.first_user_input_topic())
                                    .await;
                            }
                            if !stream_progress || !state.is_active_run() {
                                render_once(terminal, &mut state)?;
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
                            render_once(terminal, &mut state)?;
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
                    render_once(terminal, &mut state)?;
                }
                completion = model_discovery_rx.recv() => {
                    let Some(completion) = completion else {
                        continue;
                    };
                    if completion.generation == model_discovery_generation {
                        state.update_model_picker(&completion.alias, completion.result);
                        render_once(terminal, &mut state)?;
                    }
                }
                completion = clipboard_image_rx.recv() => {
                    let Some(completion) = completion else {
                        continue;
                    };
                    apply_clipboard_image_completion(completion.result, &mut state);
                    render_once(terminal, &mut state)?;
                }
                Some(completion) = command_output_rx.recv() => {
                    completion.apply(&mut state);
                    render_once(terminal, &mut state)?;
                }
                Some(completion) = archive_segment_rx.recv() => {
                    completion.apply(&mut state, &mut projector);
                    render_once(terminal, &mut state)?;
                }
                Some(completion) = compaction_rx.recv() => {
                    completion.apply(&mut state);
                    render_once(terminal, &mut state)?;
                }
                Some(completion) = open_url_rx.recv() => {
                    completion.apply(&mut state);
                    render_once(terminal, &mut state)?;
                }
            }
        }

        Ok(exit_reason)
    }.await;

    session.stream.request_cancel();
    if let Some(token) = model_discovery_token.take() {
        token.cancel();
    }
    model_discovery_rx.close();
    clipboard_image_rx.close();
    command_output_rx.close();
    archive_segment_rx.close();
    compaction_rx.close();
    open_url_rx.close();
    let session_result = session.stream.wait_until_closed().await.map_err(unexpected);
    session.discard_if_empty().await;
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
    match (run_result, cleanup_result) {
        (Ok(exit), Ok(())) => Ok(exit),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
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

fn active_hovered_link(state: &TuiState) -> Option<crate::tui::links::HoveredLink> {
    match state.overlay() {
        Some(crate::tui::overlay::Overlay::PlanView(view)) => {
            view.child_state().hovered_link().cloned()
        }
        Some(crate::tui::overlay::Overlay::SubagentInspect(takeover)) => {
            takeover.child_state().hovered_link().cloned()
        }
        Some(_) => None,
        None => state.hovered_link().cloned(),
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

fn is_user_activity_event(event: &TerminalEvent) -> bool {
    matches!(
        event,
        TerminalEvent::Key(_)
            | TerminalEvent::Mouse(_)
            | TerminalEvent::MouseScrollUp(_)
            | TerminalEvent::MouseScrollDown(_)
            | TerminalEvent::Paste(_)
    )
}

fn refresh_interactions(state: &mut TuiState, size: Size) {
    state.expire_clipboard_feedback();
    let rects = cockpit_rects(size, state);
    state.validate_text_selection_area(render::timeline_text_area(state, rects.timeline));
    state.autoscroll_text_selection();
    state.expire_idle_timeline_review();
}

fn new_refresh_interval() -> time::Interval {
    let mut interval = time::interval(TUI_REFRESH_INTERVAL);
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    interval
}

#[cfg(test)]
mod tests;
