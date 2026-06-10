use super::{RuntimeInner, diagnostic_from_text};
use crate::{session::SessionState, tool_input_validation::ToolInputValidationError};
use futures_util::StreamExt;
use merry_core::{
    ErrorInfo, PendingToolCall, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallResultStatus,
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
    text: String,
) -> bool {
    if !send_assistant_text_output_recorded_event(inner, sender, token, text).await {
        return false;
    }

    if token.is_cancelled() {
        let _ = send_cancelled_event(inner, sender).await;
        return false;
    }

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

    completed_permit.send(completed_event);
    true
}

pub(super) async fn send_assistant_text_output_recorded_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEvent>,
    token: &CancellationToken,
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
        session.record_assistant_text_output(text)
    };

    let Ok(artifact_event) = artifact_event else {
        drop(artifact_permit);
        let diagnostic = diagnostic_from_text(
            "assistant_output_artifact",
            "assistant output artifact could not be recorded",
        );
        let _ = send_failed_event(inner, sender, token, diagnostic).await;
        return false;
    };

    artifact_permit.send(artifact_event);
    true
}

pub(super) async fn send_tool_call_pending_event(
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
        session.record_tool_call_pending(call)
    };

    match event {
        Ok(event) => {
            let bridge_call = match &event.payload {
                RuntimeJournalPayload::ToolCallPending { call } => inner
                    .tool_registry
                    .registered_tool(call.name())
                    .is_some_and(|tool| tool.runner() == crate::ToolRunner::Bridge)
                    .then(|| call.clone()),
                _ => None,
            };
            permit.send(event);

            if let Some(call) = bridge_call {
                if let Some(Err(error)) = inner.tool_registry.validate_tool_input(&call) {
                    return send_bridge_tool_input_validation_failure_events(
                        inner, sender, token, &call, error,
                    )
                    .await;
                }

                send_bridge_tool_call_requested_event(inner, sender, token, call).await
            } else {
                true
            }
        }
        Err(diagnostic) => {
            drop(permit);
            send_failed_event(inner, sender, token, diagnostic).await
        }
    }
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
    if sender.is_closed() {
        return None;
    }

    tokio::select! {
        biased;
        () = sender.closed() => None,
        permit = sender.reserve() => permit.ok(),
    }
}
