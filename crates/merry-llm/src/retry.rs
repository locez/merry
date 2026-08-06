//! Provider-neutral retry policy and wrapper.

use crate::{
    ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelProvider,
    ModelProviderFuture, ModelRequest, ModelStreamContext, ProviderErrorKind,
};
use futures_util::{StreamExt, stream};
use merry_core::ProviderName;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::time::Instant;

const DEFAULT_MAX_ATTEMPTS: usize = 1;
const DEFAULT_INITIAL_DELAY: Duration = Duration::from_secs(1);
const DEFAULT_MAX_DELAY: Duration = Duration::from_secs(120);
const DEFAULT_MAX_ELAPSED: Duration = Duration::from_secs(300);
const RETRY_STREAM_BUFFER: usize = 16;

/// Provider-neutral retry policy for one model turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelRetryPolicy {
    enabled: bool,
    max_attempts: usize,
    initial_delay: Duration,
    max_delay: Duration,
    max_elapsed: Duration,
    jitter: bool,
}

impl ModelRetryPolicy {
    /// Returns a disabled retry policy.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            max_attempts: 1,
            initial_delay: DEFAULT_INITIAL_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
            max_elapsed: DEFAULT_MAX_ELAPSED,
            jitter: false,
        }
    }

    /// Returns the default coding-agent retry policy.
    #[must_use]
    pub const fn coding_agent_default() -> Self {
        Self {
            enabled: true,
            max_attempts: 6,
            initial_delay: DEFAULT_INITIAL_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
            max_elapsed: DEFAULT_MAX_ELAPSED,
            jitter: true,
        }
    }

    /// Creates a validated retry policy.
    pub fn new(
        enabled: bool,
        max_attempts: usize,
        initial_delay: Duration,
        max_delay: Duration,
        max_elapsed: Duration,
        jitter: bool,
    ) -> Result<Self, ModelRetryPolicyError> {
        if max_attempts == 0 {
            return Err(ModelRetryPolicyError::MaxAttemptsZero);
        }
        if initial_delay.is_zero() {
            return Err(ModelRetryPolicyError::InitialDelayZero);
        }
        if max_delay < initial_delay {
            return Err(ModelRetryPolicyError::MaxDelayBeforeInitial);
        }
        if max_elapsed < initial_delay {
            return Err(ModelRetryPolicyError::MaxElapsedBeforeInitial);
        }

        Ok(Self {
            enabled,
            max_attempts,
            initial_delay,
            max_delay,
            max_elapsed,
            jitter,
        })
    }

    /// Returns whether retries are enabled.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Returns the maximum number of attempts for one model turn.
    #[must_use]
    pub const fn max_attempts(self) -> usize {
        self.max_attempts
    }

    /// Returns the first retry delay.
    #[must_use]
    pub const fn initial_delay(self) -> Duration {
        self.initial_delay
    }

    /// Returns the maximum delay before a single retry.
    #[must_use]
    pub const fn max_delay(self) -> Duration {
        self.max_delay
    }

    /// Returns the maximum elapsed retry budget for one model turn.
    #[must_use]
    pub const fn max_elapsed(self) -> Duration {
        self.max_elapsed
    }

    /// Returns whether retry delays are jittered.
    #[must_use]
    pub const fn jitter(self) -> bool {
        self.jitter
    }

    /// Returns whether this policy can retry at least once.
    #[must_use]
    pub const fn can_retry(self) -> bool {
        self.enabled && self.max_attempts > 1
    }

    fn is_retryable(self, error: &ModelError) -> bool {
        if !self.can_retry() {
            return false;
        }
        matches!(
            error.kind(),
            ProviderErrorKind::InvalidToolCall
                | ProviderErrorKind::RateLimited
                | ProviderErrorKind::Unavailable
        )
    }

    fn retry_delay(self, error: &ModelError, retry_index: usize) -> Duration {
        if let Some(retry_after) = error.retry_after() {
            return retry_after.min(self.max_delay);
        }

        let shift = retry_index.saturating_sub(1).min(31) as u32;
        let delay = self
            .initial_delay
            .saturating_mul(1_u32 << shift)
            .min(self.max_delay);
        self.apply_jitter(delay)
    }

    fn apply_jitter(self, delay: Duration) -> Duration {
        if !self.jitter {
            return delay;
        }
        let millis = duration_millis_u64(delay);
        if millis <= 1 {
            return delay;
        }

        let min = (millis / 2).max(1);
        let span = millis - min + 1;
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        Duration::from_millis(min + (seed % u128::from(span)) as u64)
    }
}

impl Default for ModelRetryPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            initial_delay: DEFAULT_INITIAL_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
            max_elapsed: DEFAULT_MAX_ELAPSED,
            jitter: false,
        }
    }
}

/// Retry policy validation errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ModelRetryPolicyError {
    /// Maximum attempts must be positive.
    #[error("model retry max_attempts must be greater than zero")]
    MaxAttemptsZero,
    /// Initial delay must be positive.
    #[error("model retry initial_delay must be greater than zero")]
    InitialDelayZero,
    /// Maximum delay must not be lower than initial delay.
    #[error("model retry max_delay must be greater than or equal to initial_delay")]
    MaxDelayBeforeInitial,
    /// Maximum elapsed budget must allow at least the first retry delay.
    #[error("model retry max_elapsed must be greater than or equal to initial_delay")]
    MaxElapsedBeforeInitial,
}

/// Provider-neutral retry event emitted while resolving one model turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRetryEvent {
    /// A model provider attempt started.
    AttemptStarted { attempt: usize, max_attempts: usize },
    /// A retryable provider error scheduled another attempt.
    RetryScheduled {
        attempt: usize,
        next_attempt: usize,
        max_attempts: usize,
        delay: Duration,
        error_kind: ProviderErrorKind,
    },
    /// A retryable provider error cannot be retried because policy budget ended.
    RetryExhausted {
        attempts_run: usize,
        max_attempts: usize,
        error_kind: ProviderErrorKind,
    },
}

/// Event stream returned by a retrying model provider.
pub type ModelRetryEventStream =
    Pin<Box<dyn futures_core::Stream<Item = ModelRetryEvent> + Send + 'static>>;

/// Context passed to retrying providers.
#[derive(Default)]
pub struct RetryModelStreamContext {
    stream_context: ModelStreamContext,
    retry_events: Option<tokio::sync::mpsc::Sender<ModelRetryEvent>>,
}

impl RetryModelStreamContext {
    /// Creates context from a normal model stream context.
    #[must_use]
    pub fn new(stream_context: ModelStreamContext) -> Self {
        Self {
            stream_context,
            retry_events: None,
        }
    }

    /// Adds a retry event sink.
    #[must_use]
    pub fn with_retry_events(
        mut self,
        retry_events: tokio::sync::mpsc::Sender<ModelRetryEvent>,
    ) -> Self {
        self.retry_events = Some(retry_events);
        self
    }

    fn stream_context(&self) -> ModelStreamContext {
        self.stream_context.clone()
    }

    async fn emit(&self, event: ModelRetryEvent) {
        if let Some(sender) = &self.retry_events {
            let _ = sender.send(event).await;
        }
    }
}

/// Provider wrapper that retries a complete model turn.
#[derive(Clone)]
pub struct RetryingModelProvider {
    inner: Arc<dyn ModelProvider>,
    policy: ModelRetryPolicy,
}

impl RetryingModelProvider {
    /// Creates a retrying wrapper.
    #[must_use]
    pub fn new(inner: Arc<dyn ModelProvider>, policy: ModelRetryPolicy) -> Self {
        Self { inner, policy }
    }

    /// Returns the wrapped provider.
    #[must_use]
    pub fn inner(&self) -> Arc<dyn ModelProvider> {
        Arc::clone(&self.inner)
    }

    /// Returns the retry policy.
    #[must_use]
    pub const fn policy(&self) -> ModelRetryPolicy {
        self.policy
    }

    /// Starts a retrying model turn with retry event reporting.
    pub fn stream_model_with_retry_events<'a>(
        &'a self,
        request: ModelRequest,
        context: RetryModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.stream_context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            let (sender, receiver) = tokio::sync::mpsc::channel(RETRY_STREAM_BUFFER);
            let inner = Arc::clone(&self.inner);
            let policy = self.policy;
            tokio::spawn(async move {
                run_retry_stream(inner, policy, request, context, sender).await;
            });
            let stream = stream::unfold(receiver, |mut receiver| async move {
                receiver.recv().await.map(|item| (item, receiver))
            });
            let stream: ModelEventStream = Box::pin(stream);
            Ok(stream)
        })
    }
}

async fn run_retry_stream(
    inner: Arc<dyn ModelProvider>,
    policy: ModelRetryPolicy,
    request: ModelRequest,
    context: RetryModelStreamContext,
    sender: tokio::sync::mpsc::Sender<Result<ModelEvent, ModelError>>,
) {
    let token = context.stream_context.cancellation_token().clone();
    if !send_stream_item(&sender, &token, Ok(ModelEvent::Started)).await {
        send_cancelled_if_open(&sender);
        return;
    }

    let started = Instant::now();
    let mut attempt = 1;

    loop {
        if token.is_cancelled() {
            send_cancelled_if_open(&sender);
            return;
        }

        if policy.can_retry() {
            context
                .emit(ModelRetryEvent::AttemptStarted {
                    attempt,
                    max_attempts: policy.max_attempts,
                })
                .await;
        }

        let setup = tokio::select! {
            biased;
            () = token.cancelled() => {
                send_cancelled_if_open(&sender);
                return;
            }
            () = sender.closed() => return,
            result = inner.stream_model(request.clone(), context.stream_context()) => result,
        };

        let error = match setup {
            Ok(mut stream) => {
                let mut committed = false;
                loop {
                    let item = tokio::select! {
                        biased;
                        () = token.cancelled() => {
                            send_cancelled_if_open(&sender);
                            return;
                        }
                        () = sender.closed() => return,
                        item = stream.next() => item,
                    };

                    match item {
                        Some(Ok(ModelEvent::Started)) => {}
                        Some(Ok(event)) => {
                            committed |= commits_output(&event);
                            let completed = matches!(event, ModelEvent::Completed { .. });
                            if !send_stream_item(&sender, &token, Ok(event)).await {
                                send_cancelled_if_open(&sender);
                                return;
                            }
                            if completed {
                                return;
                            }
                        }
                        Some(Err(error)) if committed => {
                            let _ = send_stream_item(&sender, &token, Err(error)).await;
                            return;
                        }
                        Some(Err(error)) => break error,
                        None if committed => {
                            let _ = send_stream_item(
                                &sender,
                                &token,
                                Err(model_stream_ended_before_completion()),
                            )
                            .await;
                            return;
                        }
                        None => break model_stream_ended_before_completion(),
                    }
                }
            }
            Err(error) => error,
        };

        let Some(delay) = retry_delay(policy, &context, started, attempt, &error).await else {
            let _ = send_stream_item(&sender, &token, Err(error)).await;
            return;
        };

        tokio::select! {
            biased;
            () = token.cancelled() => {
                send_cancelled_if_open(&sender);
                return;
            }
            () = sender.closed() => return,
            () = tokio::time::sleep(delay) => {}
        }
        attempt += 1;
    }
}

async fn retry_delay(
    policy: ModelRetryPolicy,
    context: &RetryModelStreamContext,
    started: Instant,
    attempt: usize,
    error: &ModelError,
) -> Option<Duration> {
    if !policy.is_retryable(error) || attempt >= policy.max_attempts {
        if policy.is_retryable(error) {
            context
                .emit(ModelRetryEvent::RetryExhausted {
                    attempts_run: attempt,
                    max_attempts: policy.max_attempts,
                    error_kind: error.kind(),
                })
                .await;
        }
        return None;
    }

    let delay = policy.retry_delay(error, attempt);
    if started.elapsed().saturating_add(delay) > policy.max_elapsed {
        context
            .emit(ModelRetryEvent::RetryExhausted {
                attempts_run: attempt,
                max_attempts: policy.max_attempts,
                error_kind: error.kind(),
            })
            .await;
        return None;
    }

    context
        .emit(ModelRetryEvent::RetryScheduled {
            attempt,
            next_attempt: attempt + 1,
            max_attempts: policy.max_attempts,
            delay,
            error_kind: error.kind(),
        })
        .await;
    Some(delay)
}

fn commits_output(event: &ModelEvent) -> bool {
    matches!(
        event,
        ModelEvent::OutputTextDelta { .. }
            | ModelEvent::ToolCallRequested { .. }
            | ModelEvent::Completed { .. }
    )
}

fn model_stream_ended_before_completion() -> ModelError {
    ModelError::provider(
        ProviderErrorKind::Unavailable,
        "model stream ended before completion",
    )
}

fn send_cancelled_if_open(sender: &tokio::sync::mpsc::Sender<Result<ModelEvent, ModelError>>) {
    let _ = sender.try_send(Err(ModelError::Cancelled));
}

async fn send_stream_item(
    sender: &tokio::sync::mpsc::Sender<Result<ModelEvent, ModelError>>,
    token: &tokio_util::sync::CancellationToken,
    item: Result<ModelEvent, ModelError>,
) -> bool {
    tokio::select! {
        biased;
        () = token.cancelled() => false,
        result = sender.send(item) => result.is_ok(),
    }
}

impl ModelProvider for RetryingModelProvider {
    fn name(&self) -> &ProviderName {
        self.inner.name()
    }

    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        self.stream_model_with_retry_events(request, RetryModelStreamContext::new(context))
    }
}

fn duration_millis_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests;
