//! TUI event-loop and background-task ownership; input translation and effects are separate.

pub(super) use super::controller_provider::provider_discovery_draft;
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
        state::{TimelineItem, TuiState},
        terminal::{TerminalEvent, TerminalSession},
    },
    web::RuntimeWebService,
};
#[cfg(test)]
pub(super) use effects::persist_submitted_input_history;
pub(super) use input::project_local_effect;
pub(crate) use input::{
    apply_clipboard_image_completion, handle_key_action, handle_key_event,
    handle_mouse_scroll_down, handle_mouse_scroll_up, handle_paste_event,
};
use merry_runtime::InteractiveRunMessage;
use ratatui::layout::{Rect, Size};
use std::time::Duration;
use tokio::{sync::mpsc, task::JoinSet, time};
use tokio_util::sync::CancellationToken;

mod effects;

mod input;

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
    let mut background_tasks = JoinSet::new();
    let (model_discovery_tx, mut model_discovery_rx) = mpsc::channel(4);
    let (clipboard_image_tx, mut clipboard_image_rx) = mpsc::channel(4);
    let mut model_discovery_generation = 0_u64;
    let mut model_discovery_token: Option<CancellationToken> = None;
    let mut subagent_activity_open = true;
    let mut permission_requests_open = true;
    let mut input_history_warning_shown = false;
    let mut web_service = RuntimeWebService::new(session.runtime().clone());
    let run_result: Result<(), CliError> = async {
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
                Some(result) = background_tasks.join_next(), if !background_tasks.is_empty() => {
                    result.map_err(unexpected)?;
                }
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
                                background_tasks: &mut background_tasks,
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
                message = session.stream.next_message() => {
                    let Some(message) = message.map_err(unexpected)? else {
                        break;
                    };
                    match message {
                        InteractiveRunMessage::Event(event) => {
                            projector.apply(event, &mut state);
                            render_once(&mut terminal, &state)?;
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

        Ok(())
    }.await;

    session.stream.request_cancel();
    if let Some(token) = model_discovery_token.take() {
        token.cancel();
    }
    model_discovery_rx.close();
    clipboard_image_rx.close();
    let session_result = session.stream.wait_until_closed().await.map_err(unexpected);
    let tasks_result = drain_background_tasks(&mut background_tasks).await;
    let web_result = web_service.shutdown().await.map_err(unexpected);
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
mod tests;
