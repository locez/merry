//! Explicit runtime, persistence, clipboard, and provider effect dispatch.

use crate::{
    cli_error::{CliError, unexpected},
    tui::{
        controller::{ClipboardImageCompletion, ControllerEffect, ProviderController},
        input_history_store::InputHistoryStore,
        preferences::TuiPreferencesStore,
        runtime::TuiRuntimeSession,
        state::{TimelineItem, TuiState},
    },
    web::{RuntimeWebService, open_in_browser},
};
use merry_runtime::InterruptReason;
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
    pub(super) web_service: &'a mut RuntimeWebService,
    pub(super) background_tasks: &'a mut JoinSet<()>,
}

pub(super) async fn dispatch_effect(
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
        background_tasks,
    } = services;
    if let Some(should_quit) =
        crate::tui::plan_controller::dispatch_effect(&effect, session, state).await
    {
        return Ok(should_quit);
    }
    if crate::tui::controller_provider::is_provider_effect(&effect) {
        return crate::tui::controller_provider::dispatch_provider_effect(
            effect,
            session,
            state,
            preferences_store,
            providers,
            background_tasks,
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
            start_clipboard_image_read(clipboard_image_tx.clone(), background_tasks);
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
            background_tasks.spawn(async move {
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
            session
                .stream
                .wait_until_closed()
                .await
                .map_err(unexpected)?;
            session.save_on_exit().await?;
            Ok(true)
        }
        _ => unreachable!("provider effect handled by the provider dispatcher"),
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

#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
pub(super) fn start_clipboard_image_read(
    sender: mpsc::Sender<ClipboardImageCompletion>,
    background_tasks: &mut JoinSet<()>,
) {
    background_tasks.spawn(async move {
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
