//! Runtime builder and step execution skeleton.

use crate::{
    ArtifactContent, ContextCompiler, ContextEntry, ContextSummary, LedgerProjectionSnapshot,
    RuntimeError, RuntimeEventStream, SessionContextSnapshot,
    event_stream::ActiveStepPermit,
    session::{SessionState, is_runtime_reserved_artifact_id},
    step::{StepContext, StepInput, compile_step_model_request},
    tool::{RegisteredTool, ToolExecutionContext, ToolExecutionError, ToolRegistry},
};
use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ArtifactRef, CoreError, ErrorInfo, EvidenceLocator, EvidenceRef, PendingToolCall,
    RuntimeEvent, SessionId, ToolCallArguments, ToolCallId, ToolCallResult,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelError, ModelEvent, ModelName, ModelOutput, ModelProvider,
    ModelStreamContext, ModelToolCall, ProviderErrorKind,
};
use std::{
    num::NonZeroUsize,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::{Mutex, mpsc, mpsc::Permit};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

const DEFAULT_EVENT_BUFFER_SIZE: usize = 16;
const DIAGNOSTIC_MODEL_TOOL_CALL_INVALID: &str = "model_tool_call_invalid";
const DIAGNOSTIC_MODEL_TOOL_CALL_MISSING: &str = "model_tool_call_missing";
const DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT: &str = "model_tool_call_mixed_output";
const DIAGNOSTIC_MODEL_PARALLEL_TOOL_CALLS_UNSUPPORTED: &str =
    "model_parallel_tool_calls_unsupported";
const DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED: &str = "tool_call_result_required";
const DIAGNOSTIC_TOOL_NOT_REGISTERED: &str = "tool_not_registered";

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

        if is_runtime_reserved_artifact_id(artifact.id()) {
            return Err(RuntimeError::ReservedArtifactId {
                artifact_id: artifact.id().clone(),
            });
        }

        let mut session = self.inner.session.lock().await;
        session
            .record_artifact_events(artifact, content)
            .map_err(Into::into)
    }

    /// Resolves one pending tool call with an artifact-backed result.
    ///
    /// The artifact content is durably recorded before `ToolCallResolved` is
    /// emitted. The event carries only the artifact reference, not the payload.
    pub async fn submit_tool_result(
        &self,
        result: ToolCallResult,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        if is_runtime_reserved_artifact_id(result.artifact().id()) {
            return Err(RuntimeError::ReservedArtifactId {
                artifact_id: result.artifact().id().clone(),
            });
        }

        let mut session = self.inner.session.lock().await;
        session.submit_tool_result(result, content)
    }

    /// Executes one pending tool call through a runtime-registered executor.
    ///
    /// Runtime code owns the resulting artifact id and `ToolCallResult`.
    /// Executor infrastructure errors and cancellation leave the call pending.
    pub async fn execute_tool_call(
        &self,
        call_id: &ToolCallId,
        context: ToolExecutionContext,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: self.inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }

        let pending = {
            let session = self.inner.session.lock().await;
            session
                .pending_tool_call(call_id)
                .ok_or_else(|| RuntimeError::UnknownToolCall {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                })?
        };

        let Some(executor) = self.inner.tool_registry.executor(pending.name()) else {
            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }

            let diagnostic = diagnostic_from_text(
                DIAGNOSTIC_TOOL_NOT_REGISTERED,
                format!("tool {} is not registered", pending.name()),
            );
            let content = ArtifactContent::json(format!(
                r#"{{"error":"tool_not_registered","tool":"{}"}}"#,
                pending.name()
            ));
            let mut session = self.inner.session.lock().await;
            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            return session.submit_tool_execution_outcome(
                call_id,
                merry_core::ToolCallResultStatus::Failed,
                content,
                Some(diagnostic),
            );
        };

        let execution = tokio::select! {
            biased;
            () = context.cancellation_token().cancelled() => {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            execution = executor.execute(pending, context.clone()) => execution,
        };

        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: self.inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }

        let outcome = match execution {
            Ok(outcome) => outcome,
            Err(ToolExecutionError::Cancelled) => {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            Err(ToolExecutionError::Infrastructure { message }) => {
                return Err(RuntimeError::ToolExecutionFailed {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                    message,
                });
            }
        };

        let (status, content, diagnostic) = outcome.into_parts();
        let mut session = self.inner.session.lock().await;
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: self.inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }
        session.submit_tool_execution_outcome(call_id, status, content, diagnostic)
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

    /// Returns a snapshot of provider-neutral tool calls currently awaiting results.
    pub async fn pending_tool_calls(&self) -> Vec<PendingToolCall> {
        let session = self.inner.session.lock().await;
        session.pending_tool_calls()
    }
}

/// Builder for a Merry runtime.
pub struct RuntimeBuilder {
    session_id: SessionId,
    event_buffer_size: NonZeroUsize,
    model_provider: Option<ModelProviderConfig>,
    registered_tools: Vec<RegisteredTool>,
}

impl RuntimeBuilder {
    fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            event_buffer_size: NonZeroUsize::new(DEFAULT_EVENT_BUFFER_SIZE)
                .expect("default event buffer size is non-zero"),
            model_provider: None,
            registered_tools: Vec::new(),
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

    /// Registers a runtime-owned tool executor.
    #[must_use]
    pub fn register_tool(mut self, tool: RegisteredTool) -> Self {
        self.registered_tools.push(tool);
        self
    }

    /// Builds the runtime.
    pub fn build(self) -> Result<Runtime, RuntimeError> {
        let tool_registry =
            ToolRegistry::from_registered(self.registered_tools).map_err(|duplicate| {
                RuntimeError::DuplicateToolRegistration {
                    name: duplicate.name,
                }
            })?;

        Ok(Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: self.session_id.clone(),
                session: Mutex::new(SessionState::new(self.session_id)),
                active_step: Arc::new(AtomicBool::new(false)),
                event_buffer_size: self.event_buffer_size,
                model_provider: self.model_provider,
                tool_registry,
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
    tool_registry: ToolRegistry,
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
    if has_unresolved_pending_tool_calls(inner).await {
        let diagnostic = diagnostic_from_text(
            DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED,
            "a pending tool call must be resolved before the next provider step",
        );
        let _ = send_failed_event(inner, sender, token, diagnostic).await;
        return;
    }

    let (snapshot, continuations) = {
        let session = inner.session.lock().await;
        let continuations = match session.unconsumed_tool_continuation_snapshots() {
            Ok(continuations) => continuations,
            Err(error) => {
                let diagnostic = diagnostic_from_text(
                    "tool_continuation_artifact",
                    format!("tool continuation artifact could not be read: {error}"),
                );
                drop(session);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        };
        (session.context_snapshot(), continuations)
    };
    let sent_continuation_count = continuations.len();
    let tool_specs = inner.tool_registry.tool_specs();

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
        &continuations,
        tool_specs,
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

    let mut saw_non_empty_text_delta = false;
    let mut streamed_tool_call: Option<PendingToolCall> = None;

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
            Some(Ok(ModelEvent::Started)) => {}
            Some(Ok(ModelEvent::OutputTextDelta { delta })) => {
                if !delta.is_empty() {
                    saw_non_empty_text_delta = true;
                }
            }
            Some(Ok(ModelEvent::Completed { response })) => match response.finish_reason() {
                FinishReason::Stop => {
                    if streamed_tool_call.is_some() {
                        let diagnostic = diagnostic_from_text(
                            DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
                            "model requested a tool call before completing with text output",
                        );
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        return;
                    }

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
                        sent_continuation_count,
                    )
                    .await
                    {
                        let _ = send_cancelled_if_requested(inner, sender, token).await;
                    }
                    return;
                }
                FinishReason::ToolCalls => {
                    if saw_non_empty_text_delta {
                        let diagnostic = diagnostic_from_text(
                            DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
                            "model emitted text before requesting a tool call",
                        );
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        return;
                    }

                    match pending_tool_call_from_outputs(
                        response.outputs(),
                        streamed_tool_call.as_ref(),
                    ) {
                        Ok(call) => {
                            if !send_tool_call_pending_event(
                                inner,
                                sender,
                                token,
                                call,
                                sent_continuation_count,
                            )
                            .await
                            {
                                let _ = send_cancelled_if_requested(inner, sender, token).await;
                            }
                        }
                        Err(diagnostic) => {
                            let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        }
                    }
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
            Some(Ok(ModelEvent::ToolCallRequested { call })) => {
                if saw_non_empty_text_delta {
                    let diagnostic = diagnostic_from_text(
                        DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
                        "model emitted text before requesting a tool call",
                    );
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }

                match pending_tool_call_from_model(&call)
                    .and_then(|call| record_streamed_tool_call(&mut streamed_tool_call, call))
                {
                    Ok(call) => {
                        streamed_tool_call = Some(call);
                    }
                    Err(diagnostic) => {
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        return;
                    }
                }
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

async fn has_unresolved_pending_tool_calls(inner: &RuntimeInner) -> bool {
    let session = inner.session.lock().await;
    session.has_pending_tool_calls()
}

async fn send_assistant_text_output_completed_events(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    text: String,
    sent_continuation_count: usize,
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
        match session.record_assistant_text_output(text) {
            Ok(event) => {
                session.consume_tool_continuations(sent_continuation_count);
                Ok(event)
            }
            Err(error) => Err(error),
        }
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

async fn send_tool_call_pending_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    call: PendingToolCall,
    sent_continuation_count: usize,
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
        match session.record_tool_call_pending(call) {
            Ok(event) => {
                session.consume_tool_continuations(sent_continuation_count);
                Ok(event)
            }
            Err(diagnostic) => Err(diagnostic),
        }
    };

    match event {
        Ok(event) => {
            permit.send(event);
            true
        }
        Err(diagnostic) => {
            drop(permit);
            send_failed_event(inner, sender, token, diagnostic).await
        }
    }
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

fn record_streamed_tool_call(
    streamed_tool_call: &mut Option<PendingToolCall>,
    call: PendingToolCall,
) -> Result<PendingToolCall, ErrorInfo> {
    match streamed_tool_call.as_ref() {
        Some(existing) if existing == &call => Ok(existing.clone()),
        Some(_) => Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_PARALLEL_TOOL_CALLS_UNSUPPORTED,
            "model requested multiple streamed tool calls, but runtime policy only supports one pending tool call",
        )),
        None => Ok(call),
    }
}

fn pending_tool_call_from_outputs(
    outputs: &[ModelOutput],
    streamed_tool_call: Option<&PendingToolCall>,
) -> Result<PendingToolCall, ErrorInfo> {
    if outputs.is_empty() {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_MISSING,
            "model finished with tool calls but returned no tool call output",
        ));
    }

    let tool_call_count = outputs
        .iter()
        .filter(|output| matches!(output, ModelOutput::ToolCall { .. }))
        .count();
    let text_output_count = outputs
        .iter()
        .filter(|output| matches!(output, ModelOutput::Text { .. }))
        .count();

    if tool_call_count == 0 {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_MISSING,
            "model finished with tool calls but returned no tool call output",
        ));
    }

    if tool_call_count > 1 {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_PARALLEL_TOOL_CALLS_UNSUPPORTED,
            "model returned multiple tool calls, but runtime policy only supports one pending tool call",
        ));
    }

    if text_output_count > 0 || outputs.len() != 1 {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
            "model returned text and a tool call in the same response",
        ));
    }

    let [ModelOutput::ToolCall { call }] = outputs else {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_MISSING,
            "model finished with tool calls but returned no tool call output",
        ));
    };

    let completed_call = pending_tool_call_from_model(call)?;
    match streamed_tool_call {
        Some(streamed_call) if streamed_call == &completed_call => Ok(streamed_call.clone()),
        Some(_) => Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_PARALLEL_TOOL_CALLS_UNSUPPORTED,
            "model completed with a different tool call than the streamed pending call",
        )),
        None => Ok(completed_call),
    }
}

fn pending_tool_call_from_model(call: &ModelToolCall) -> Result<PendingToolCall, ErrorInfo> {
    let id = ToolCallId::new(call.id().as_str()).map_err(tool_call_conversion_diagnostic)?;
    let arguments = ToolCallArguments::new(call.arguments().as_object().clone());
    Ok(PendingToolCall::new(id, call.name().clone(), arguments))
}

fn tool_call_conversion_diagnostic(error: CoreError) -> ErrorInfo {
    diagnostic_from_text(
        DIAGNOSTIC_MODEL_TOOL_CALL_INVALID,
        format!("model tool call could not be normalized: {error}"),
    )
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
    use super::{Runtime, RuntimeInner, send_cancelled_event};
    use crate::session::SessionState;
    use crate::tool::{ToolExecutionContext, ToolRegistry};
    use merry_core::{PendingToolCall, SessionId, ToolCallArguments, ToolCallId, ToolName};
    use std::{
        num::NonZeroUsize,
        sync::{Arc, atomic::AtomicBool},
    };
    use tokio::sync::{Mutex, mpsc, oneshot};
    use tokio_util::sync::CancellationToken;

    fn runtime_inner() -> RuntimeInner {
        let session_id = SessionId::new("runtime-send-test").expect("valid session id");
        RuntimeInner {
            session_id: session_id.clone(),
            session: Mutex::new(SessionState::new(session_id)),
            active_step: Arc::new(AtomicBool::new(false)),
            event_buffer_size: NonZeroUsize::new(1).expect("non-zero buffer"),
            model_provider: None,
            tool_registry: ToolRegistry::default(),
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

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_unregistered_tool_while_waiting_to_submit_keeps_pending() {
        let session_id =
            SessionId::new("runtime-unregistered-submit-cancel").expect("valid session id");
        let call_id = ToolCallId::new("call-unregistered").expect("valid tool call id");
        let pending = PendingToolCall::new(
            call_id.clone(),
            ToolName::new("missing_tool").expect("valid tool name"),
            ToolCallArguments::new(Default::default()),
        );
        let runtime = Runtime::builder(session_id)
            .build()
            .expect("runtime should build");

        let mut initial_session_guard = runtime.inner.session.lock().await;
        initial_session_guard
            .record_tool_call_pending(pending.clone())
            .expect("pending call should record");
        let projection_before = initial_session_guard.ledger_projection();

        let token = CancellationToken::new();
        let execute_runtime = runtime.clone();
        let execute_call_id = call_id.clone();
        let execute_token = token.clone();
        let execute_handle = tokio::spawn(async move {
            execute_runtime
                .execute_tool_call(&execute_call_id, ToolExecutionContext::new(execute_token))
                .await
        });
        tokio::task::yield_now().await;

        let (lock_acquired_sender, lock_acquired_receiver) = oneshot::channel();
        let (release_lock_sender, release_lock_receiver) = oneshot::channel();
        let blocker_runtime = runtime.clone();
        let blocker_handle = tokio::spawn(async move {
            let _session_guard = blocker_runtime.inner.session.lock().await;
            let _ = lock_acquired_sender.send(());
            let _ = release_lock_receiver.await;
        });
        tokio::task::yield_now().await;

        drop(initial_session_guard);
        lock_acquired_receiver
            .await
            .expect("blocker should acquire the session lock after pending lookup");
        tokio::task::yield_now().await;

        token.cancel();
        release_lock_sender
            .send(())
            .expect("blocker should still be waiting for release");

        let err = execute_handle
            .await
            .expect("tool execution task should not panic")
            .expect_err("cancelled unregistered tool execution should not resolve pending");
        blocker_handle
            .await
            .expect("session lock blocker should not panic");

        assert!(matches!(
            err,
            crate::RuntimeError::ToolExecutionCancelled { call_id: cancelled, .. }
                if cancelled == call_id
        ));
        assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
        assert_eq!(runtime.ledger_projection().await, projection_before);
    }
}
