use super::{RuntimeInner, journal_emission::reserve_normal_event_slot};
use crate::events::RuntimeJournalEventBatch;
use futures_util::StreamExt;
use merry_core::RuntimeJournalPayload;
use merry_llm::{
    ModelError, ModelEvent, ModelEventStream, ModelProvider, ModelRequest, ModelRetryEvent,
    ModelRetryPolicy, ModelStreamContext, ProviderErrorKind, RetryModelStreamContext,
    RetryingModelProvider,
};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) async fn wait_for_retrying_stream_setup<F>(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
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
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
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
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
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
    inner.emit_journal_batch(permit, event.into());
    true
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
        ProviderErrorKind::InvalidToolCall => "invalid_tool_call",
        ProviderErrorKind::Cancelled => "cancelled",
        ProviderErrorKind::Authentication => "authentication",
        ProviderErrorKind::RateLimited => "rate_limited",
        ProviderErrorKind::Unavailable => "unavailable",
        ProviderErrorKind::Protocol => "protocol",
        ProviderErrorKind::Other => "other",
    }
}
