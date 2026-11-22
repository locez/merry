//! Explicit runtime, persistence, clipboard, and provider effect dispatch.

#[cfg(feature = "web")]
use crate::web::{RuntimeWebService, open_in_browser};
use crate::{
    cli_error::{CliError, unexpected},
    tui::{
        controller::{
            ClipboardImageCompletion, CommandOutputCompletion, CompactionCompletion,
            ControllerEffect, ControllerExit, ProviderController,
        },
        input_history_store::InputHistoryStore,
        preferences::TuiPreferencesStore,
        runtime::TuiRuntimeSession,
        state::{TimelineItem, TuiState},
    },
};
use merry_runtime::{InteractiveError, InterruptReason, QueuedInputId};
use tokio::{sync::mpsc, task::JoinSet};

pub(super) struct InputHistoryController<'a> {
    pub(super) store: &'a InputHistoryStore,
    pub(super) warning_shown: &'a mut bool,
}

pub(super) struct ControllerServices<'a> {
    pub(super) preferences_store: &'a TuiPreferencesStore,
    pub(super) input_history: InputHistoryController<'a>,
    pub(super) providers: ProviderController<'a>,
    pub(super) clipboard_image_tx: &'a mpsc::Sender<ClipboardImageCompletion>,
    pub(super) command_output_tx: &'a mpsc::Sender<CommandOutputCompletion>,
    pub(super) compaction_tx: &'a mpsc::Sender<CompactionCompletion>,
    pub(super) terminal: &'a mut crate::tui::terminal::TerminalSession,
    #[cfg(feature = "web")]
    pub(super) web_service: &'a mut RuntimeWebService,
    pub(super) background_tasks: &'a mut JoinSet<()>,
    pub(super) subagent_live_stream: &'a mut Option<(
        String,
        tokio::sync::broadcast::Receiver<merry_core::RuntimeEvent>,
    )>,
}

pub(super) async fn dispatch_effect(
    effect: ControllerEffect,
    session: &mut TuiRuntimeSession,
    state: &mut TuiState,
    services: ControllerServices<'_>,
) -> Result<Option<ControllerExit>, CliError> {
    let ControllerServices {
        preferences_store,
        input_history,
        providers,
        clipboard_image_tx,
        command_output_tx,
        compaction_tx,
        terminal,
        #[cfg(feature = "web")]
        web_service,
        background_tasks,
        subagent_live_stream,
    } = services;
    if crate::tui::plan_controller::dispatch_effect(&effect, session, state).await {
        return Ok(None);
    }
    if crate::tui::controller_provider::is_provider_effect(&effect) {
        let should_quit = crate::tui::controller_provider::dispatch_provider_effect(
            effect,
            session,
            state,
            preferences_store,
            providers,
            background_tasks,
        )
        .await?;
        return Ok(should_quit.then_some(ControllerExit::Quit));
    }
    match effect {
        ControllerEffect::None => Ok(None),
        ControllerEffect::LoadCommandOutput(call_id) => {
            let runtime = session.runtime().clone();
            let sender = command_output_tx.clone();
            let generation = state.command_details_generation();
            background_tasks.spawn(async move {
                let result = crate::tui::command_details::load_output(&runtime, &call_id).await;
                let _ = sender
                    .send(CommandOutputCompletion::new(call_id, generation, result))
                    .await;
            });
            Ok(None)
        }
        ControllerEffect::CopyText(text) => {
            let system = write_system_clipboard(&text).await;
            let terminal_result = terminal.copy_text(&text);
            let (feedback, failed) = crate::tui::copy_controls::copy_result_feedback(
                system.is_ok(),
                terminal_result.is_ok(),
                system.as_ref().err().map(String::as_str).unwrap_or(""),
            );
            show_copy_feedback(state, feedback, failed);
            Ok(None)
        }
        ControllerEffect::CopyTextTooLarge => {
            show_copy_feedback(
                state,
                "Copy failed: selected text exceeds the 1 MiB clipboard limit".to_owned(),
                true,
            );
            Ok(None)
        }
        ControllerEffect::Submit(submission) => {
            let (message, history_text) = submission
                .into_user_message_and_history()
                .map_err(unexpected)?;
            session
                .input
                .submit_message(message)
                .await
                .map_err(unexpected)?;
            persist_submitted_input_history(
                input_history.store,
                state,
                &history_text,
                input_history.warning_shown,
            )
            .await;
            Ok(None)
        }
        ControllerEffect::SetPendingPaused(paused) => {
            if let Err(error) = session.input.set_paused(paused).await {
                tracing::debug!(error = %error, paused, "pending pause update ignored");
            }
            Ok(None)
        }
        ControllerEffect::UpdatePendingInput { id, text, resume } => {
            match session
                .input
                .update_by_id(QueuedInputId::from_u64(id), &text)
                .await
            {
                Ok(()) => {}
                Err(
                    InteractiveError::AlreadyAccepted
                    | InteractiveError::AlreadyRemoved
                    | InteractiveError::UnknownInput,
                ) => {}
                Err(error) => {
                    state.push_timeline_item(TimelineItem::Diagnostic {
                        title: "Pending edit failed".to_owned(),
                        body: error.to_string(),
                    });
                }
            }
            if resume && let Err(error) = session.input.set_paused(false).await {
                tracing::debug!(error = %error, "pending resume after edit ignored");
            }
            Ok(None)
        }
        ControllerEffect::RemovePendingInput { id, resume } => {
            match session
                .input
                .remove_by_id(QueuedInputId::from_u64(id))
                .await
            {
                Ok(()) => {}
                Err(
                    InteractiveError::AlreadyAccepted
                    | InteractiveError::AlreadyRemoved
                    | InteractiveError::UnknownInput,
                ) => {}
                Err(error) => {
                    tracing::debug!(error = %error, id, "pending remove ignored");
                }
            }
            if let Some(resume) = resume
                && let Err(error) = session.input.set_paused(!resume).await
            {
                tracing::debug!(error = %error, resume, "pending pause restore after remove ignored");
            }
            Ok(None)
        }
        ControllerEffect::FlushPendingInputs => {
            session.input.flush().await.map_err(unexpected)?;
            Ok(None)
        }
        ControllerEffect::PasteImage => {
            start_clipboard_image_read(clipboard_image_tx.clone(), background_tasks);
            Ok(None)
        }
        #[cfg(feature = "web")]
        ControllerEffect::OpenSessionInBrowser => {
            let url = match web_service.session_url(&session.metadata.session_id).await {
                Ok(url) => url,
                Err(error) => {
                    state.push_timeline_item(TimelineItem::Diagnostic {
                        title: "Web service unavailable".to_owned(),
                        body: error.to_string(),
                    });
                    return Ok(None);
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
            Ok(None)
        }
        ControllerEffect::ApprovePermission(approval_id) => {
            if let Some(next) = session.resolve_permission_review(&approval_id, true)? {
                state.open_permission_review_view(next);
            }
            Ok(None)
        }
        ControllerEffect::DenyPermission(approval_id) => {
            if let Some(next) = session.resolve_permission_review(&approval_id, false)? {
                state.open_permission_review_view(next);
            }
            Ok(None)
        }
        ControllerEffect::Interrupt => {
            let control = session.control.clone();
            background_tasks.spawn(async move {
                if let Err(error) = control.interrupt(InterruptReason::User).await {
                    tracing::warn!(error = %error, "interactive interrupt request failed");
                }
            });
            Ok(None)
        }
        ControllerEffect::ShowTasks => {
            let processes = session.runtime().background_processes();
            state.sync_background_processes(processes.clone());
            let subagents = session.runtime().subagent_snapshot().await;
            if let Some(views) = subagents.as_deref() {
                state.apply_subagent_status_views(views);
            }
            state.push_timeline_item(TimelineItem::LocalCommand {
                title: "Tasks".to_owned(),
                body: crate::tui::tasks::tasks_body(&processes, subagents.as_deref()),
            });
            Ok(None)
        }
        ControllerEffect::RefreshSubagents => {
            state.sync_background_processes(session.runtime().background_processes());
            if let Some(views) = session.runtime().subagent_snapshot().await {
                state.apply_subagent_status_views(&views);
            }
            if let Some(agent_id) = state.inspected_subagent_id()
                && let Ok(id) = merry_core::SubagentId::new(&agent_id)
                && let Some(inspect) = session.runtime().inspect_subagent(&id).await
            {
                state.load_subagent_inspect_transcript(
                    &agent_id,
                    &inspect.task,
                    &inspect.transcript,
                );
                *subagent_live_stream = Some((agent_id, inspect.events));
            }
            Ok(None)
        }
        ControllerEffect::SendSubagentMessage { agent_id, message } => {
            if let Ok(id) = merry_core::SubagentId::new(&agent_id) {
                let already_subscribed = subagent_live_stream
                    .as_ref()
                    .is_some_and(|(current_id, _)| current_id == &agent_id);
                if !already_subscribed
                    && let Some(inspect) = session.runtime().inspect_subagent(&id).await
                {
                    *subagent_live_stream = Some((agent_id.clone(), inspect.events));
                }
                if let Err(error) = session.runtime().send_subagent_message(&id, message).await {
                    state.apply_subagent_live_event(
                        &agent_id,
                        merry_core::RuntimeEvent::RunFailed {
                            diagnostic: merry_core::ErrorInfo::new(
                                "subagent_message_failed",
                                &error.to_string(),
                            )
                            .expect("static diagnostic code is valid"),
                            source: merry_core::RuntimeEventSource::new(
                                session.metadata.session_id.clone(),
                                0,
                            ),
                        },
                    );
                } else if let Some(views) = session.runtime().subagent_snapshot().await {
                    state.apply_subagent_status_views(&views);
                }
            }
            Ok(None)
        }
        ControllerEffect::CancelSubagent(agent_id) => {
            if let Ok(id) = merry_core::SubagentId::new(&agent_id) {
                let _ = session.runtime().cancel_subagent(&id).await;
                if let Some(views) = session.runtime().subagent_snapshot().await {
                    state.apply_subagent_status_views(&views);
                }
            }
            Ok(None)
        }
        ControllerEffect::InspectBackgroundProcess(process_id) => {
            state.sync_background_processes(session.runtime().background_processes());
            match merry_runtime::BackgroundProcessId::new(process_id.clone()) {
                Ok(id) => match session.runtime().read_background_process(&id) {
                    Ok(output) => state.update_background_task_output(output),
                    Err(err) => state.update_background_task_error(&process_id, err.message()),
                },
                Err(message) => state.update_background_task_error(&process_id, message),
            }
            Ok(None)
        }
        ControllerEffect::StopBackgroundProcess(process_id) => {
            if let Ok(id) = merry_runtime::BackgroundProcessId::new(process_id.clone()) {
                let _ = session.runtime().stop_background_process(&id);
                if state.inspected_background_process_id().as_deref() == Some(process_id.as_str()) {
                    match session.runtime().read_background_process(&id) {
                        Ok(output) => state.update_background_task_output(output),
                        Err(err) => state.update_background_task_error(&process_id, err.message()),
                    }
                }
            }
            state.sync_background_processes(session.runtime().background_processes());
            Ok(None)
        }
        ControllerEffect::CompactContext { focus } => {
            let control = session.control.clone();
            let sender = compaction_tx.clone();
            background_tasks.spawn(async move {
                let result = control.compact_context(focus).await;
                let _ = sender.send(CompactionCompletion::new(result)).await;
            });
            Ok(None)
        }
        ControllerEffect::SaveSession => {
            session.set_topic_if_empty(state.first_user_input_topic());
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
            Ok(None)
        }
        ControllerEffect::PersistPreferences(preferences) => {
            preferences_store
                .save(&preferences)
                .await
                .map_err(unexpected)?;
            Ok(None)
        }
        ControllerEffect::ApplyRuntimePreferences(preferences) => {
            session.apply_preferences(&preferences).await?;
            preferences_store
                .save(&preferences)
                .await
                .map_err(unexpected)?;
            state.set_model_label(session.model_label.clone());
            state.set_reasoning_effort_label(session.reasoning_effort_label.clone());
            Ok(None)
        }
        ControllerEffect::SetPlanMode { .. }
        | ControllerEffect::ApprovePlan { .. }
        | ControllerEffect::RequestPlanChanges { .. }
        | ControllerEffect::AbandonPlan(_) => unreachable!("plan effect handled above"),
        ControllerEffect::NewSession => {
            let should_save = session.resumed || state.first_user_input_topic().is_some();
            close_session(session, state, should_save).await?;
            Ok(Some(ControllerExit::NewSession))
        }
        ControllerEffect::ResumeSession => {
            let should_save = session.resumed || state.first_user_input_topic().is_some();
            close_session(session, state, should_save).await?;
            Ok(Some(ControllerExit::ResumeSession))
        }
        ControllerEffect::Quit => {
            close_session(session, state, true).await?;
            Ok(Some(ControllerExit::Quit))
        }
        _ => unreachable!("provider effect handled by the provider dispatcher"),
    }
}

async fn close_session(
    session: &mut TuiRuntimeSession,
    state: &TuiState,
    save_session: bool,
) -> Result<(), CliError> {
    session.set_topic_if_empty(state.first_user_input_topic());
    session.control.close().await.map_err(unexpected)?;
    session
        .stream
        .wait_until_closed()
        .await
        .map_err(unexpected)?;
    if save_session {
        session.save_on_exit().await?;
    }
    Ok(())
}

async fn write_system_clipboard(text: &str) -> Result<(), String> {
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = text;
        Err("system clipboard is unavailable".to_owned())
    }
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        let text = text.to_owned();
        match tokio::time::timeout(
            std::time::Duration::from_millis(300),
            tokio::task::spawn_blocking(move || {
                let mut clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
                clipboard.set_text(text).map_err(|error| error.to_string())
            }),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(format!("clipboard task failed: {error}")),
            Err(_) => Err("clipboard timed out".to_owned()),
        }
    }
}

fn show_copy_feedback(state: &mut TuiState, feedback: String, failed: bool) {
    if let Some(crate::tui::overlay::Overlay::CommandDetails(details)) = state.overlay_mut() {
        details.set_feedback(feedback);
    } else {
        state.show_clipboard_feedback(feedback, failed);
    }
}

pub(in crate::tui) async fn persist_submitted_input_history(
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

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(super) fn start_clipboard_image_read(
    sender: mpsc::Sender<ClipboardImageCompletion>,
    background_tasks: &mut JoinSet<()>,
) {
    background_tasks.spawn(async move {
        let result = tokio::task::spawn_blocking(|| {
            crate::tui::clipboard_image::read_clipboard_image()
                .map_err(|error| error.to_string())
                .and_then(|image| image.into_draft_image().map_err(|error| error.to_string()))
        })
        .await
        .unwrap_or_else(|error| Err(format!("clipboard image task failed: {error}")));
        let _ = sender.send(ClipboardImageCompletion { result }).await;
    });
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(super) fn start_clipboard_image_read(
    sender: mpsc::Sender<ClipboardImageCompletion>,
    background_tasks: &mut JoinSet<()>,
) {
    background_tasks.spawn(async move {
        let _ = sender
            .send(ClipboardImageCompletion {
                result: Err(
                    "clipboard image paste is currently supported only on Linux, macOS, and Windows"
                        .to_owned(),
                ),
            })
            .await;
    });
}
