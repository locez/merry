//! Runtime builder and step execution skeleton.

use crate::{
    ArtifactContent, ContextCompiler, ContextEntry, ContextSummary, LedgerProjectionSnapshot,
    RuntimeError, RuntimeEventStream, SessionContextSnapshot,
    event_stream::ActiveStepPermit,
    session::SessionState,
    step::{StepContext, StepInput, compile_step_model_request},
};
use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ArtifactRef, ErrorInfo, EvidenceLocator, EvidenceRef, RuntimeEvent, SessionId,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelError, ModelEvent, ModelName, ModelOutput, ModelProvider,
    ModelStreamContext, ProviderErrorKind,
};
use std::{
    num::NonZeroUsize,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::{Mutex, mpsc, mpsc::Permit};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

const DEFAULT_EVENT_BUFFER_SIZE: usize = 16;

/// Merry runtime handle for one session.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl Runtime {
    /// Creates a runtime builder for the provided session.
    #[must_use]
    pub fn builder(session_id: SessionId) -> RuntimeBuilder {
        RuntimeBuilder::new(session_id)
    }

    /// Starts a runtime step and returns its event stream.
    pub fn step(
        &self,
        input: StepInput,
        context: StepContext,
    ) -> Result<RuntimeEventStream, RuntimeError> {
        let active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        let (parent_token, generation_config) = context.into_parts();
        let step_token = parent_token.child_token();
        let producer_token = step_token.clone();
        let (sender, receiver) = mpsc::channel(self.inner.event_buffer_size.get());
        let inner = Arc::clone(&self.inner);

        let producer_handle = tokio::spawn(async move {
            run_step(
                inner,
                sender,
                producer_token,
                input,
                generation_config,
                active_permit,
            )
            .await;
        });

        Ok(RuntimeEventStream::new(
            ReceiverStream::new(receiver),
            step_token,
            producer_handle,
        ))
    }

    /// Records exact artifact state into the owning session and returns observable events.
    ///
    /// When this is the first observable action in the session, `SessionStarted`
    /// is returned before `ArtifactRecorded`.
    pub async fn record_artifact(
        &self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        let mut session = self.inner.session.lock().await;
        session
            .record_artifact_events(artifact, content)
            .map_err(Into::into)
    }

    /// Creates an exact evidence reference from artifact state owned by this session.
    pub async fn evidence_ref(
        &self,
        artifact_id: &ArtifactId,
        locator: EvidenceLocator,
    ) -> Result<EvidenceRef, RuntimeError> {
        let session = self.inner.session.lock().await;
        session
            .evidence_ref(artifact_id, locator)
            .map_err(Into::into)
    }

    /// Records a structured context entry into the owning session.
    pub async fn record_context_entry(&self, entry: ContextEntry) {
        let mut session = self.inner.session.lock().await;
        session.record_context_entry(entry);
    }

    /// Records a summary context entry into the owning session.
    pub async fn record_context_summary(&self, summary: ContextSummary) {
        self.record_context_entry(ContextEntry::summary(summary))
            .await
    }

    /// Builds a sealed context snapshot from session-owned context and artifacts.
    pub async fn context_snapshot(&self) -> SessionContextSnapshot {
        let session = self.inner.session.lock().await;
        session.context_snapshot()
    }

    /// Builds a read-only deterministic projection of the task ledger.
    pub async fn ledger_projection(&self) -> LedgerProjectionSnapshot {
        let session = self.inner.session.lock().await;
        session.ledger_projection()
    }
}

/// Builder for a Merry runtime.
pub struct RuntimeBuilder {
    session_id: SessionId,
    event_buffer_size: NonZeroUsize,
    model_provider: Option<ModelProviderConfig>,
}

impl RuntimeBuilder {
    fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            event_buffer_size: NonZeroUsize::new(DEFAULT_EVENT_BUFFER_SIZE)
                .expect("default event buffer size is non-zero"),
            model_provider: None,
        }
    }

    /// Sets the bounded event channel buffer size.
    #[must_use]
    pub fn event_buffer_size(mut self, event_buffer_size: NonZeroUsize) -> Self {
        self.event_buffer_size = event_buffer_size;
        self
    }

    /// Sets the provider and model used by runtime steps.
    #[must_use]
    pub fn model_provider(mut self, provider: Arc<dyn ModelProvider>, model: ModelName) -> Self {
        self.model_provider = Some(ModelProviderConfig { provider, model });
        self
    }

    /// Builds the runtime.
    pub fn build(self) -> Result<Runtime, RuntimeError> {
        Ok(Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: self.session_id.clone(),
                session: Mutex::new(SessionState::new(self.session_id)),
                active_step: Arc::new(AtomicBool::new(false)),
                event_buffer_size: self.event_buffer_size,
                model_provider: self.model_provider,
            }),
        })
    }
}

#[derive(Clone)]
struct ModelProviderConfig {
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
}

struct RuntimeInner {
    session_id: SessionId,
    session: Mutex<SessionState>,
    active_step: Arc<AtomicBool>,
    event_buffer_size: NonZeroUsize,
    model_provider: Option<ModelProviderConfig>,
}

async fn run_step(
    inner: Arc<RuntimeInner>,
    sender: mpsc::Sender<RuntimeEvent>,
    token: CancellationToken,
    input: StepInput,
    generation_config: GenerationConfig,
    _active_permit: ActiveStepPermit,
) {
    if token.is_cancelled() {
        let _ = send_cancelled_event(&inner, &sender).await;
        return;
    }

    if !send_normal_event(&inner, &sender, &token, |session| {
        session.record_session_started_if_needed()
    })
    .await
    {
        let _ = send_cancelled_if_requested(&inner, &sender, &token).await;
        return;
    }

    if token.is_cancelled() {
        let _ = send_cancelled_event(&inner, &sender).await;
        return;
    }

    if !send_normal_event(&inner, &sender, &token, |session| {
        Some(session.record_step_started())
    })
    .await
    {
        let _ = send_cancelled_if_requested(&inner, &sender, &token).await;
        return;
    }

    if token.is_cancelled() {
        let _ = send_cancelled_event(&inner, &sender).await;
        return;
    }

    let Some(provider_config) = inner.model_provider.clone() else {
        if !send_normal_event(&inner, &sender, &token, |session| {
            Some(session.record_step_completed())
        })
        .await
        {
            let _ = send_cancelled_if_requested(&inner, &sender, &token).await;
        }
        return;
    };

    run_provider_step(
        &inner,
        &sender,
        &token,
        input,
        generation_config,
        provider_config,
    )
    .await;
}

async fn run_provider_step(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    input: StepInput,
    generation_config: GenerationConfig,
    provider_config: ModelProviderConfig,
) {
    let snapshot = {
        let session = inner.session.lock().await;
        session.context_snapshot()
    };

    let compiled_context = match ContextCompiler::new().compile(&snapshot) {
        Ok(context) => context,
        Err(error) => {
            let diagnostic = diagnostic_from_text("context_compile", error.to_string());
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    let request = match compile_step_model_request(
        &input,
        &provider_config.model,
        &compiled_context,
        generation_config,
    ) {
        Ok(request) => request,
        Err(error) => {
            let diagnostic = diagnostic_from_text("model_request", error.to_string());
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    let stream_context = ModelStreamContext::new(token.clone());
    let stream_result = tokio::select! {
        biased;
        () = token.cancelled() => {
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }
        result = provider_config.provider.stream_model(request, stream_context) => result,
    };

    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            if is_cancelled_model_error(&error) {
                let _ = send_cancelled_event(inner, sender).await;
                return;
            }

            let diagnostic = diagnostic_from_model_error(error);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    loop {
        let item = tokio::select! {
            biased;
            () = token.cancelled() => {
                let _ = send_cancelled_event(inner, sender).await;
                return;
            }
            item = stream.next() => item,
        };

        match item {
            Some(Ok(ModelEvent::Started | ModelEvent::OutputTextDelta { .. })) => {}
            Some(Ok(ModelEvent::Completed { response })) => match response.finish_reason() {
                FinishReason::Stop => {
                    let [ModelOutput::Text { text }] = response.outputs() else {
                        let diagnostic = diagnostic_from_text(
                            "model_output_unsupported",
                            "model stop output must contain exactly one text item",
                        );
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        return;
                    };

                    if !send_assistant_text_output_completed_events(
                        inner,
                        sender,
                        token,
                        text.clone(),
                    )
                    .await
                    {
                        let _ = send_cancelled_if_requested(inner, sender, token).await;
                    }
                    return;
                }
                FinishReason::ToolCalls => {
                    let diagnostic = diagnostic_from_text(
                        "model_tool_call_requested",
                        "model completed with tool calls, but runtime tool execution is not supported yet",
                    );
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }
                FinishReason::Length => {
                    let diagnostic = diagnostic_from_text(
                        "model_length",
                        "model output stopped because it reached a length limit",
                    );
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }
                FinishReason::Cancelled => {
                    let _ = send_cancelled_event(inner, sender).await;
                    return;
                }
                FinishReason::Error => {
                    let diagnostic = diagnostic_from_text(
                        "model_finish_error",
                        "model output stopped because the provider reported a finish error",
                    );
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }
            },
            Some(Ok(ModelEvent::ToolCallRequested { .. })) => {
                let diagnostic = diagnostic_from_text(
                    "model_tool_call_requested",
                    "model requested a tool call, but runtime tool execution is not supported yet",
                );
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
            Some(Err(error)) => {
                if is_cancelled_model_error(&error) {
                    let _ = send_cancelled_event(inner, sender).await;
                    return;
                }

                let diagnostic = diagnostic_from_model_error(error);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
            None => {
                let diagnostic = diagnostic_from_text(
                    "model_stream_eof",
                    "model stream ended before completion",
                );
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        }
    }
}

async fn send_assistant_text_output_completed_events(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
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

async fn send_failed_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
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

fn is_cancelled_model_error(error: &ModelError) -> bool {
    error.kind() == ProviderErrorKind::Cancelled
}

fn diagnostic_from_model_error(error: ModelError) -> ErrorInfo {
    let code = match error.kind() {
        ProviderErrorKind::InvalidRequest => "model_invalid_request",
        ProviderErrorKind::Cancelled => "cancelled",
        ProviderErrorKind::Authentication => "model_authentication",
        ProviderErrorKind::RateLimited => "model_rate_limited",
        ProviderErrorKind::Unavailable => "model_unavailable",
        ProviderErrorKind::Protocol => "model_protocol",
        ProviderErrorKind::Other => "model_other",
    };

    diagnostic_from_text(code, error.to_string())
}

fn diagnostic_from_text(code: &'static str, message: impl AsRef<str>) -> ErrorInfo {
    let message = sanitize_diagnostic_message(message.as_ref());
    ErrorInfo::new(code, &message).expect("runtime diagnostic is sanitized and uses static code")
}

fn sanitize_diagnostic_message(message: &str) -> String {
    let sanitized: String = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let trimmed = sanitized.trim();

    let source = if trimmed.is_empty() {
        "provider returned an empty error message"
    } else {
        trimmed
    };

    source.chars().take(4096).collect()
}

async fn send_normal_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    make_event: impl FnOnce(&mut SessionState) -> Option<RuntimeEvent>,
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
    sender: &'a mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
) -> Option<Permit<'a, RuntimeEvent>> {
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

async fn send_cancelled_if_requested(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
) -> bool {
    if !token.is_cancelled() {
        return false;
    }

    send_cancelled_event(inner, sender).await
}

async fn send_cancelled_event(inner: &RuntimeInner, sender: &mpsc::Sender<RuntimeEvent>) -> bool {
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
    sender: &'a mpsc::Sender<RuntimeEvent>,
) -> Option<Permit<'a, RuntimeEvent>> {
    if sender.is_closed() {
        return None;
    }

    tokio::select! {
        biased;
        () = sender.closed() => None,
        permit = sender.reserve() => permit.ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeInner, send_cancelled_event};
    use crate::session::SessionState;
    use merry_core::SessionId;
    use std::{
        num::NonZeroUsize,
        sync::{Arc, atomic::AtomicBool},
    };
    use tokio::sync::{Mutex, mpsc};

    fn runtime_inner() -> RuntimeInner {
        let session_id = SessionId::new("runtime-send-test").expect("valid session id");
        RuntimeInner {
            session_id: session_id.clone(),
            session: Mutex::new(SessionState::new(session_id)),
            active_step: Arc::new(AtomicBool::new(false)),
            event_buffer_size: NonZeroUsize::new(1).expect("non-zero buffer"),
            model_provider: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_event_send_returns_false_when_channel_is_closed() {
        let inner = runtime_inner();
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);

        let sent = send_cancelled_event(&inner, &sender).await;
        let projection = {
            let session = inner.session.lock().await;
            session.ledger_projection()
        };

        assert!(!sent);
        assert!(projection.entries().is_empty());
    }
}
