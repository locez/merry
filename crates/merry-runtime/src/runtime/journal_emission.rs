use super::{RuntimeInner, diagnostic_from_text, persist_resume_safe_savepoint_if_configured};
use crate::{
    session::{ModelTurnId, SessionState},
    tool_input_validation::ToolInputValidationError,
};
use futures_util::StreamExt;
use merry_core::{
    CompactionUsageWindow, ErrorInfo, ModelUsage, PendingToolCall, RuntimeJournalEvent,
    RuntimeJournalPayload, ToolCallResultStatus, UsageContextWindow,
};
use merry_llm::{
    ModelError, ModelEvent, ModelEventStream, ModelProvider, ModelRequest, ModelRetryEvent,
    ModelRetryPolicy, ModelStreamContext, ProviderErrorKind, RetryModelStreamContext,
    RetryingModelProvider,
};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::{mpsc, mpsc::Permit};
use tokio_util::sync::CancellationToken;

pub(super) async fn send_assistant_text_output_completed_events(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    turn_id: ModelTurnId,
    text: String,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(artifact_permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let artifact_event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session
            .record_assistant_text_output(turn_id, text)
            .and_then(|event| {
                session.close_model_response(turn_id, false)?;
                Ok(event)
            })
    };

    let Ok(artifact_event) = artifact_event else {
        drop(artifact_permit);
        abort_model_turn_before_terminal_event(inner, turn_id).await;
        let diagnostic = diagnostic_from_text(
            "assistant_output_artifact",
            "assistant output artifact or model turn could not be recorded",
        );
        return send_failed_event(inner, sender, token, diagnostic).await;
    };
    artifact_permit.send(artifact_event);

    let Some(completed_permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };
    let completed_event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            drop(completed_permit);
            let _ = send_cancelled_event(inner, sender).await;
            return false;
        }
        session.record_step_completed()
    };

    persist_resume_safe_savepoint_if_configured(inner).await;
    completed_permit.send(completed_event);
    true
}

pub(super) async fn send_assistant_text_output_delta_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    delta: String,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.record_transient_event(RuntimeJournalPayload::AssistantOutputDelta { delta })
    };

    permit.send(event);
    true
}

pub(super) async fn send_model_tool_call_response_events(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    turn_id: ModelTurnId,
    commentary: Option<String>,
    calls: Vec<PendingToolCall>,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let events = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.record_model_tool_call_response(turn_id, commentary, calls)
    };

    match events {
        Ok((commentary_event, tool_event)) => {
            let bridge_calls = match &tool_event.payload {
                RuntimeJournalPayload::ToolCallPending { call } => {
                    if inner
                        .tool_registry
                        .registered_tool(call.name())
                        .is_some_and(|tool| tool.runner() == crate::ToolRunner::Bridge)
                    {
                        vec![call.clone()]
                    } else {
                        Vec::new()
                    }
                }
                RuntimeJournalPayload::ToolCallBatchPending { batch } => batch
                    .calls()
                    .iter()
                    .filter(|call| {
                        inner
                            .tool_registry
                            .registered_tool(call.name())
                            .is_some_and(|tool| tool.runner() == crate::ToolRunner::Bridge)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            if let Some(commentary_event) = commentary_event {
                permit.send(commentary_event);
                let Some(tool_permit) = reserve_event_slot_ignoring_cancellation(sender).await
                else {
                    return false;
                };
                tool_permit.send(tool_event);
            } else {
                permit.send(tool_event);
            }

            if token.is_cancelled() {
                return false;
            }

            for call in bridge_calls {
                if let Some(Err(error)) = inner.tool_registry.validate_tool_input(&call) {
                    if !send_bridge_tool_input_validation_failure_events(
                        inner, sender, token, &call, error,
                    )
                    .await
                    {
                        return false;
                    }
                } else if !send_bridge_tool_call_requested_event(inner, sender, token, call).await {
                    return false;
                }
            }
            true
        }
        Err(diagnostic) => {
            drop(permit);
            abort_model_turn_before_terminal_event(inner, turn_id).await;
            send_failed_event(inner, sender, token, diagnostic).await
        }
    }
}

async fn abort_model_turn_before_terminal_event(inner: &RuntimeInner, turn_id: ModelTurnId) {
    let mut session = inner.session.lock().await;
    if let Err(error) = session.abort_model_turn(turn_id) {
        tracing::error!(
            category = "model_turn_abort",
            model_turn_id = turn_id.as_u64(),
            error = %error,
            "failed to abort model turn before terminal journal event"
        );
    }
}

pub(super) async fn send_model_usage_updated_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    model_usage: ModelUsage,
    context: Option<UsageContextWindow>,
    compaction: Option<CompactionUsageWindow>,
) -> Result<bool, ErrorInfo> {
    if token.is_cancelled() {
        return Ok(false);
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return Ok(false);
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return Ok(false);
        }
        session.record_model_usage(model_usage, context, compaction)?
    };

    permit.send(event);
    Ok(true)
}

pub(super) async fn send_compaction_started_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.record_compaction_started()
    };

    permit.send(event);
    true
}

pub(super) async fn send_compaction_completed_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    checkpoint_id: String,
    covered_history_item_count: usize,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.record_compaction_completed(checkpoint_id, covered_history_item_count)
    };

    permit.send(event);
    true
}

async fn send_bridge_tool_call_requested_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    call: PendingToolCall,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.record_bridge_tool_call_requested(call)
    };

    permit.send(event);
    true
}

async fn send_bridge_tool_input_validation_failure_events(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    call: &PendingToolCall,
    error: ToolInputValidationError,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let result = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.submit_tool_execution_outcome(
            call.id(),
            ToolCallResultStatus::Failed,
            error.content_for_call(call),
            Some(error.diagnostic()),
            None,
        )
    };

    match result {
        Ok(events) => {
            let mut events = events.into_iter();
            let Some(artifact_event) = events.next() else {
                return false;
            };
            let Some(resolved_event) = events.next() else {
                return false;
            };
            debug_assert!(events.next().is_none());

            persist_resume_safe_savepoint_if_configured(inner).await;

            let Some(artifact_permit) = reserve_normal_event_slot(sender, token).await else {
                return false;
            };
            artifact_permit.send(artifact_event);

            let Some(resolved_permit) = reserve_normal_event_slot(sender, token).await else {
                return false;
            };
            resolved_permit.send(resolved_event);
            true
        }
        Err(error) => {
            let diagnostic =
                diagnostic_from_text("tool_input_validation_result", error.to_string());
            send_failed_event(inner, sender, token, diagnostic).await
        }
    }
}

pub(super) async fn wait_for_retrying_stream_setup<F>(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    setup: F,
    retry_events: &mut mpsc::Receiver<ModelRetryEvent>,
) -> Option<Result<ModelEventStream, ModelError>>
where
    F: Future<Output = Result<ModelEventStream, ModelError>>,
{
    tokio::pin!(setup);
    loop {
        tokio::select! {
            biased;
            () = token.cancelled() => return None,
            Some(event) = retry_events.recv() => {
                if !send_model_retry_event(inner, sender, token, event).await {
                    return None;
                }
            }
            result = &mut setup => return Some(result),
        }
    }
}

pub(super) async fn stream_model_with_retry_policy(
    provider: Arc<dyn ModelProvider>,
    retry_policy: ModelRetryPolicy,
    request: ModelRequest,
    stream_context: ModelStreamContext,
    retry_events: Option<mpsc::Sender<ModelRetryEvent>>,
) -> Result<ModelEventStream, ModelError> {
    if retry_policy.can_retry() {
        let provider = RetryingModelProvider::new(provider, retry_policy);
        let mut context = RetryModelStreamContext::new(stream_context);
        if let Some(events) = retry_events {
            context = context.with_retry_events(events);
        }
        provider
            .stream_model_with_retry_events(request, context)
            .await
    } else {
        provider.stream_model(request, stream_context).await
    }
}

pub(super) async fn wait_for_model_stream_item(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    stream: &mut Pin<Box<dyn futures_core::Stream<Item = Result<ModelEvent, ModelError>> + Send>>,
    retry_events: &mut mpsc::Receiver<ModelRetryEvent>,
) -> Option<Option<Result<ModelEvent, ModelError>>> {
    loop {
        tokio::select! {
            biased;
            () = token.cancelled() => return None,
            Some(event) = retry_events.recv() => {
                if !send_model_retry_event(inner, sender, token, event).await {
                    return None;
                }
            }
            item = stream.next() => return Some(item),
        }
    }
}

async fn send_model_retry_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    event: ModelRetryEvent,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };
    let kind = runtime_event_kind_from_model_retry_event(event);
    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.record_model_retry_event(kind)
    };
    permit.send(event);
    true
}

pub(super) async fn send_failed_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    diagnostic: ErrorInfo,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    send_normal_event(inner, sender, token, |session| {
        Some(session.record_failed(diagnostic))
    })
    .await
}

fn runtime_event_kind_from_model_retry_event(event: ModelRetryEvent) -> RuntimeJournalPayload {
    match event {
        ModelRetryEvent::AttemptStarted {
            attempt,
            max_attempts,
        } => RuntimeJournalPayload::ModelRetryAttemptStarted {
            attempt,
            max_attempts,
        },
        ModelRetryEvent::RetryScheduled {
            attempt,
            next_attempt,
            max_attempts,
            delay,
            error_kind,
        } => RuntimeJournalPayload::ModelRetryScheduled {
            attempt,
            next_attempt,
            max_attempts,
            delay_ms: duration_millis_u64(delay),
            error_kind: provider_error_kind_label(error_kind).to_owned(),
        },
        ModelRetryEvent::RetryExhausted {
            attempts_run,
            max_attempts,
            error_kind,
        } => RuntimeJournalPayload::ModelRetryExhausted {
            attempts_run,
            max_attempts,
            error_kind: provider_error_kind_label(error_kind).to_owned(),
        },
    }
}

fn duration_millis_u64(duration: std::time::Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn provider_error_kind_label(kind: ProviderErrorKind) -> &'static str {
    match kind {
        ProviderErrorKind::InvalidRequest => "invalid_request",
        ProviderErrorKind::Cancelled => "cancelled",
        ProviderErrorKind::Authentication => "authentication",
        ProviderErrorKind::RateLimited => "rate_limited",
        ProviderErrorKind::Unavailable => "unavailable",
        ProviderErrorKind::Protocol => "protocol",
        ProviderErrorKind::Other => "other",
    }
}

pub(super) fn trace_provider_step_failed(diagnostic: &ErrorInfo) {
    tracing::debug!(
        category = "failed",
        diagnostic_code = diagnostic.code(),
        "runtime provider step failed"
    );
}

pub(super) fn trace_provider_step_cancelled() {
    tracing::debug!(
        category = "cancelled",
        diagnostic_code = "cancelled",
        "runtime provider step cancelled"
    );
}

pub(super) async fn send_normal_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
    make_event: impl FnOnce(&mut SessionState) -> Option<RuntimeJournalEvent>,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        make_event(&mut session)
    };

    if let Some(event) = event {
        permit.send(event);
    }

    true
}

async fn reserve_normal_event_slot<'a>(
    sender: &'a mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
) -> Option<Permit<'a, RuntimeJournalEvent>> {
    if token.is_cancelled() || sender.is_closed() {
        return None;
    }

    tokio::select! {
        biased;
        () = token.cancelled() => None,
        () = sender.closed() => None,
        permit = sender.reserve() => permit.ok(),
    }
}

pub(super) async fn send_cancelled_if_requested(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
) -> bool {
    if !token.is_cancelled() {
        return false;
    }

    send_cancelled_event(inner, sender).await
}

pub(super) async fn send_cancelled_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
) -> bool {
    let Some(permit) = reserve_cancelled_event_slot(sender).await else {
        return false;
    };

    if sender.is_closed() {
        return false;
    }

    let diagnostic = ErrorInfo::new("cancelled", "runtime step cancelled")
        .expect("static cancellation diagnostic is valid");
    let event = {
        let mut session = inner.session.lock().await;
        session.record_cancelled(diagnostic)
    };
    permit.send(event);
    true
}

async fn reserve_cancelled_event_slot<'a>(
    sender: &'a mpsc::Sender<RuntimeJournalEvent>,
) -> Option<Permit<'a, RuntimeJournalEvent>> {
    reserve_event_slot_ignoring_cancellation(sender).await
}

async fn reserve_event_slot_ignoring_cancellation<'a>(
    sender: &'a mpsc::Sender<RuntimeJournalEvent>,
) -> Option<Permit<'a, RuntimeJournalEvent>> {
    if sender.is_closed() {
        return None;
    }

    tokio::select! {
        biased;
        () = sender.closed() => None,
        permit = sender.reserve() => permit.ok(),
    }
}
