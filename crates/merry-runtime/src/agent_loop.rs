//! Runtime-owned serial agent loop.
//!
//! The MVP loop composes existing runtime primitives:
//! [`Runtime::step`] -> [`Runtime::execute_tool_call`] -> continuation
//! [`Runtime::step`]. It is intentionally serial, provider-neutral, and bounded.

use crate::{
    FinalOutput, FinalOutputContract, Runtime, RuntimeError, RuntimeJournalEventStream,
    StepContext, StepInput, ToolExecutionContext,
    events::{ActiveStepPermit, RuntimeEventProjector},
};
use futures_core::Stream;
use futures_util::StreamExt;
use merry_core::{
    ArtifactKind, ErrorInfo, PendingToolCall, RuntimeEvent, RuntimeJournalEvent,
    RuntimeJournalPayload, SessionId, SessionUsage, ToolCallId, ToolCallResult,
    ToolCallResultStatus, ToolName,
};
use std::{
    collections::BTreeSet,
    num::NonZeroUsize,
    pin::Pin,
    task::{Context as TaskContext, Poll},
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

/// Generic SDK/runtime default for one top-level agent run.
pub const DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS: usize = 128;
/// Coding-agent default for one top-level user task.
pub const DEFAULT_CODING_AGENT_MAX_MODEL_TURNS: usize = 1024;

/// Configuration for [`Runtime::run_agent_loop`].
///
/// `max_model_turns` bounds the number of model turns started by one loop run.
/// Context compaction may happen within the run, but it does not reset this
/// control-flow and cost budget.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLoopConfig {
    max_model_turns: NonZeroUsize,
    final_output_contract: Option<FinalOutputContract>,
}

impl AgentLoopConfig {
    /// Creates loop configuration with a non-zero model-turn budget.
    pub fn new(max_model_turns: usize) -> Result<Self, AgentLoopConfigError> {
        let Some(max_model_turns) = NonZeroUsize::new(max_model_turns) else {
            return Err(AgentLoopConfigError::MaxModelTurnsMustBeNonZero);
        };

        Ok(Self {
            max_model_turns,
            final_output_contract: None,
        })
    }

    /// Maximum number of model turns this loop may start.
    #[must_use]
    pub fn max_model_turns(&self) -> usize {
        self.max_model_turns.get()
    }

    /// Adds a runtime-owned structured final-output contract.
    #[must_use]
    pub fn with_final_output_contract(mut self, contract: FinalOutputContract) -> Self {
        self.final_output_contract = Some(contract);
        self
    }

    /// Borrows the configured structured final-output contract.
    #[must_use]
    pub fn final_output_contract(&self) -> Option<&FinalOutputContract> {
        self.final_output_contract.as_ref()
    }
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            max_model_turns: NonZeroUsize::new(DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS)
                .expect("default agent loop model-turn budget is non-zero"),
            final_output_contract: None,
        }
    }
}

/// Invalid agent loop configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AgentLoopConfigError {
    /// A loop without a model-turn budget would either do no useful work or
    /// hide a caller configuration mistake.
    #[error("agent loop max_model_turns must be greater than zero")]
    MaxModelTurnsMustBeNonZero,
}

/// Result of a completed or policy-blocked agent loop run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLoopResult {
    status: AgentLoopStatus,
    events: Vec<RuntimeJournalEvent>,
    model_turns_run: usize,
    final_output: Option<String>,
    final_output_json: Option<FinalOutput>,
    session_usage: Option<SessionUsage>,
}

impl AgentLoopResult {
    fn new(
        status: AgentLoopStatus,
        events: Vec<RuntimeJournalEvent>,
        model_turns_run: usize,
        final_output: Option<String>,
        session_usage: Option<SessionUsage>,
    ) -> Self {
        Self::new_with_final_output_json(
            status,
            events,
            model_turns_run,
            final_output,
            None,
            session_usage,
        )
    }

    fn new_with_final_output_json(
        status: AgentLoopStatus,
        events: Vec<RuntimeJournalEvent>,
        model_turns_run: usize,
        final_output: Option<String>,
        final_output_json: Option<FinalOutput>,
        session_usage: Option<SessionUsage>,
    ) -> Self {
        Self {
            status,
            events,
            model_turns_run,
            final_output,
            final_output_json,
            session_usage,
        }
    }

    /// Final loop status.
    #[must_use]
    pub fn status(&self) -> &AgentLoopStatus {
        &self.status
    }

    /// Runtime events collected in emission order.
    #[must_use]
    pub fn events(&self) -> &[RuntimeJournalEvent] {
        &self.events
    }

    /// Number of model turns started by the loop.
    #[must_use]
    pub fn model_turns_run(&self) -> usize {
        self.model_turns_run
    }

    /// Explicit final text returned by the model at loop completion, when present.
    #[must_use]
    pub fn final_output(&self) -> Option<&str> {
        self.final_output.as_deref()
    }

    /// Structured JSON final output recorded by the runtime final-output tool.
    #[must_use]
    pub fn final_output_json(&self) -> Option<&FinalOutput> {
        self.final_output_json.as_ref()
    }

    /// Latest session usage snapshot when this loop result was produced.
    #[must_use]
    pub fn session_usage(&self) -> Option<&SessionUsage> {
        self.session_usage.as_ref()
    }

    /// Consumes the result and returns the collected events.
    #[must_use]
    pub fn into_events(self) -> Vec<RuntimeJournalEvent> {
        self.events
    }
}

/// Live event stream for one runtime-owned agent loop.
///
/// Polling yields runtime events as they become observable. [`Self::result`]
/// drains any unconsumed events before returning the collected loop result.
pub struct AgentLoopEventStream {
    session_id: SessionId,
    events: ReceiverStream<AgentLoopStreamMessage>,
    loop_token: tokio_util::sync::CancellationToken,
    producer_handle: Option<tokio::task::JoinHandle<()>>,
    result_receiver: Option<oneshot::Receiver<Option<AgentLoopResult>>>,
    bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
}

impl AgentLoopEventStream {
    fn new(
        session_id: SessionId,
        events: ReceiverStream<AgentLoopStreamMessage>,
        loop_token: tokio_util::sync::CancellationToken,
        producer_handle: tokio::task::JoinHandle<()>,
        result_receiver: oneshot::Receiver<Option<AgentLoopResult>>,
        bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
    ) -> Self {
        Self {
            session_id,
            events,
            loop_token,
            producer_handle: Some(producer_handle),
            result_receiver: Some(result_receiver),
            bridge_sender,
        }
    }

    /// Submits a result for a bridge tool call requested by this stream.
    ///
    /// Bridge execution happens in host SDK code, but the runtime stream owns
    /// the loop continuation. The submitted result is recorded under the same
    /// active loop permit before the loop starts the next provider step.
    pub async fn submit_bridge_tool_result(
        &self,
        result: ToolCallResult,
        content: crate::ArtifactContent,
    ) -> Result<(), RuntimeError> {
        let call_id = result.call_id().clone();
        let (ack_sender, ack_receiver) = oneshot::channel();
        let command = BridgeToolResultCommand {
            result,
            content,
            ack_sender,
        };
        self.bridge_sender
            .send(command)
            .await
            .map_err(|_| RuntimeError::UnknownToolCall {
                session_id: self.session_id.clone(),
                call_id: call_id.clone(),
            })?;
        ack_receiver
            .await
            .map_err(|_| RuntimeError::UnknownToolCall {
                session_id: self.session_id.clone(),
                call_id,
            })?
    }

    /// Returns the collected loop result once the stream has completed.
    ///
    /// Any unconsumed events are drained before waiting for the result.
    pub async fn result(&mut self) -> Option<AgentLoopResult> {
        while self.next().await.is_some() {}

        self.result_receiver.take()?.await.ok().flatten()
    }

    /// Returns the next internal driver message.
    ///
    /// SDK bridge drivers use this to execute bridge tool calls without
    /// exposing bridge handoff as a public [`RuntimeEvent`].
    pub async fn next_driver_message(&mut self) -> Option<AgentLoopStreamMessage> {
        self.events.next().await
    }
}

struct BridgeToolResultCommand {
    result: ToolCallResult,
    content: crate::ArtifactContent,
    ack_sender: oneshot::Sender<Result<(), RuntimeError>>,
}

impl Stream for AgentLoopEventStream {
    type Item = RuntimeEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.events).poll_next(cx) {
                Poll::Ready(Some(AgentLoopStreamMessage::Event(event))) => {
                    return Poll::Ready(Some(event));
                }
                Poll::Ready(Some(AgentLoopStreamMessage::BridgeToolRequest { .. })) => continue,
                Poll::Ready(None) => {
                    self.producer_handle.take();
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Drop for AgentLoopEventStream {
    fn drop(&mut self) {
        self.loop_token.cancel();
        if let Some(handle) = self.producer_handle.take() {
            handle.abort();
        }
    }
}

/// Internal agent-loop stream driver message.
// Keep the public event inline to preserve the stable stream API and avoid per-event allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentLoopStreamMessage {
    /// Public SDK/UI event.
    Event(RuntimeEvent),
    /// Bridge tool request for SDK-internal execution.
    BridgeToolRequest {
        /// Tool call that must be executed by the bridge host.
        call: PendingToolCall,
    },
}

impl AgentLoopStreamMessage {
    #[must_use]
    pub fn as_event(&self) -> Option<&RuntimeEvent> {
        match self {
            Self::Event(event) => Some(event),
            Self::BridgeToolRequest { .. } => None,
        }
    }

    #[must_use]
    pub fn as_bridge_tool_request(&self) -> Option<&PendingToolCall> {
        match self {
            Self::BridgeToolRequest { call } => Some(call),
            Self::Event(_) => None,
        }
    }
}

/// Terminal or blocked status for an agent loop run.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLoopStatus {
    /// The provider returned a final completed step.
    Completed,
    /// The runtime emitted a failed event. This is distinct from a method error
    /// returned by [`Runtime::step`] or [`Runtime::execute_tool_call`].
    Failed { diagnostic: ErrorInfo },
    /// The runtime emitted a cancelled event, or loop-owned tool execution was
    /// cancelled before producing a durable result.
    Cancelled { diagnostic: ErrorInfo },
    /// The loop stopped because MVP loop policy cannot safely continue.
    Blocked { reason: AgentLoopBlockedReason },
}

/// Reasons a loop can stop without final model completion.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLoopBlockedReason {
    /// The configured model-turn budget has been reached.
    MaxModelTurnsReached { max_model_turns: usize },
    /// Legacy blocked status retained for compatibility with older loop results.
    ///
    /// Current runtime-owned loops execute supported pending batches directly.
    MultiplePendingToolCalls { pending_count: usize },
    /// A step emitted both completion and pending tool-call state.
    StepCompletedWithPendingToolCall { pending_count: usize },
    /// A step stream ended without a completion, failure, cancellation, or
    /// pending tool-call event.
    StepEndedWithoutTerminalEvent,
    /// The loop required the final-output tool but the model completed with text.
    FinalOutputToolNotCalled,
    /// A pending tool call must be executed by an external bridge runner.
    BridgeToolCallRequested {
        /// Bridge tool call id.
        call_id: ToolCallId,
        /// Bridge tool name.
        tool_name: ToolName,
    },
}

/// Runtime method error returned while an agent loop was running.
///
/// Runtime failed/cancelled events are represented as [`AgentLoopStatus`].
/// This error is reserved for facade-method failures such as step admission,
/// unknown calls, or executor infrastructure failure. Cooperative tool
/// cancellation is represented as [`AgentLoopStatus::Cancelled`]. The
/// already-observed runtime events are preserved for callers.
#[derive(Debug, Error)]
#[error("agent loop stopped on runtime method error: {source}")]
pub struct AgentLoopError {
    events: Vec<RuntimeJournalEvent>,
    #[source]
    source: RuntimeError,
}

impl AgentLoopError {
    fn new(events: Vec<RuntimeJournalEvent>, source: RuntimeError) -> Self {
        Self { events, source }
    }

    /// Runtime events collected before the method error.
    #[must_use]
    pub fn events(&self) -> &[RuntimeJournalEvent] {
        &self.events
    }

    /// Underlying runtime method error.
    #[must_use]
    pub fn runtime_error(&self) -> &RuntimeError {
        &self.source
    }

    /// Consumes the error into its preserved events and runtime error.
    #[must_use]
    pub fn into_parts(self) -> (Vec<RuntimeJournalEvent>, RuntimeError) {
        (self.events, self.source)
    }
}

impl Runtime {
    /// Runs a bounded runtime-owned agent loop.
    ///
    /// The loop starts with one [`Runtime::step`]. If the step completes, fails,
    /// or is cancelled, the corresponding status is returned with all observed
    /// events. If the step records pending tool calls and more step budget
    /// remains, the loop executes registered runtime tools, appends their
    /// events, and starts a continuation step without adding a new user
    /// message.
    ///
    /// A batch preserves model order around exclusive tools. Adjacent tools
    /// explicitly registered as parallel-safe may execute concurrently up to
    /// the runtime limit; all other tools execute serially. The loop does not
    /// introduce provider conversation state. It owns the runtime active-step
    /// permit for the full step -> tool execution -> continuation sequence.
    /// While the loop is running, cloned runtime handles reject concurrent
    /// direct mutation APIs with [`RuntimeError::StepAlreadyActive`].
    /// Cancellation and generation controls are reused from `context` for
    /// every step and tool execution.
    pub async fn run_agent_loop(
        &self,
        input: StepInput,
        context: StepContext,
        config: AgentLoopConfig,
    ) -> Result<AgentLoopResult, AgentLoopError> {
        let loop_permit = self
            .acquire_active_step_permit()
            .map_err(|source| AgentLoopError::new(Vec::new(), source))?;
        let loop_token = context.cancellation_token().clone();
        let generation_config = context.generation_config().clone();
        let mut next_input = Some(input);
        let mut events = Vec::new();
        let mut model_turns_run = 0;

        tracing::info!(
            event = "runtime.loop.start",
            session_id = self.session_id().as_str(),
            max_model_turns = config.max_model_turns(),
            "runtime loop start"
        );

        loop {
            let input = next_input
                .take()
                .expect("agent loop always installs the next step input before continuing");
            let step_index = model_turns_run + 1;
            tracing::info!(
                event = "runtime.step.start",
                session_id = self.session_id().as_str(),
                step_index,
                "runtime loop step start"
            );
            let mut step_context = StepContext::new(loop_token.clone())
                .with_generation_config(generation_config.clone());
            if let Some(contract) = config.final_output_contract().cloned() {
                step_context = step_context.with_final_output_contract(contract);
            }
            let stream =
                match self.step_with_active_permit(input, step_context, loop_permit.clone()) {
                    Ok(stream) => stream,
                    Err(source) => {
                        trace_loop_error(self.session_id().as_str(), model_turns_run, &source);
                        return Err(AgentLoopError::new(events, source));
                    }
                };
            model_turns_run += 1;

            let mut step_events = collect_step_events(stream).await;
            let step_final_output = final_assistant_output_from_step(self, &step_events).await;
            let outcome = classify_step_events(&step_events, config.final_output_contract());
            events.append(&mut step_events);

            match outcome {
                StepOutcome::Completed => {
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "completed",
                        model_turns_run,
                        None,
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Completed,
                        events,
                        model_turns_run,
                        step_final_output,
                        session_usage,
                    ));
                }
                StepOutcome::Failed(diagnostic) => {
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "failed",
                        model_turns_run,
                        Some(diagnostic.code()),
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Failed { diagnostic },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }
                StepOutcome::Cancelled(diagnostic) => {
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "cancelled",
                        model_turns_run,
                        Some(diagnostic.code()),
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Cancelled { diagnostic },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }
                StepOutcome::Blocked(reason) => {
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "blocked",
                        model_turns_run,
                        Some(blocked_reason_code(&reason)),
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Blocked { reason },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }
                StepOutcome::Pending(PendingLoopToolCall::Bridge(call)) => {
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "blocked",
                        model_turns_run,
                        Some("bridge_tool_call_requested"),
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Blocked {
                            reason: AgentLoopBlockedReason::BridgeToolCallRequested {
                                call_id: call.id().clone(),
                                tool_name: call.name().clone(),
                            },
                        },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }
                StepOutcome::ToolResultRecorded => {
                    if model_turns_run >= config.max_model_turns() {
                        trace_loop_finish(
                            self.session_id().as_str(),
                            "blocked",
                            model_turns_run,
                            Some("max_model_turns_reached"),
                        );
                        let session_usage = self.usage().await;
                        return Ok(AgentLoopResult::new(
                            AgentLoopStatus::Blocked {
                                reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                    max_model_turns: config.max_model_turns(),
                                },
                            },
                            events,
                            model_turns_run,
                            None,
                            session_usage,
                        ));
                    }

                    next_input = Some(continuation_step_input());
                }
                StepOutcome::Pending(PendingLoopToolCall::FinalOutput(call)) => {
                    if let Some(Err(error)) = config
                        .final_output_contract()
                        .map(|contract| contract.validate_call(&call))
                    {
                        let mut failure_events = match self
                            .submit_tool_input_validation_failure_with_active_permit(
                                &call,
                                error,
                                &loop_permit,
                            )
                            .await
                        {
                            Ok(events) => events,
                            Err(source) => {
                                trace_loop_error(
                                    self.session_id().as_str(),
                                    model_turns_run,
                                    &source,
                                );
                                return Err(AgentLoopError::new(events, source));
                            }
                        };
                        events.append(&mut failure_events);

                        if model_turns_run >= config.max_model_turns() {
                            trace_loop_finish(
                                self.session_id().as_str(),
                                "blocked",
                                model_turns_run,
                                Some("max_model_turns_reached"),
                            );
                            let session_usage = self.usage().await;
                            return Ok(AgentLoopResult::new(
                                AgentLoopStatus::Blocked {
                                    reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                        max_model_turns: config.max_model_turns(),
                                    },
                                },
                                events,
                                model_turns_run,
                                None,
                                session_usage,
                            ));
                        }

                        next_input = Some(continuation_step_input());
                        continue;
                    }

                    let (final_output, mut final_events) =
                        match record_final_output_tool_call(self, call).await {
                            Ok(recorded) => recorded,
                            Err(source) => {
                                trace_loop_error(
                                    self.session_id().as_str(),
                                    model_turns_run,
                                    &source,
                                );
                                return Err(AgentLoopError::new(events, source));
                            }
                        };
                    events.append(&mut final_events);
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "completed",
                        model_turns_run,
                        None,
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new_with_final_output_json(
                        AgentLoopStatus::Completed,
                        events,
                        model_turns_run,
                        None,
                        Some(final_output),
                        session_usage,
                    ));
                }
                StepOutcome::PendingBatch(calls) => {
                    if model_turns_run >= config.max_model_turns() {
                        let session_usage = self.usage().await;
                        return Ok(AgentLoopResult::new(
                            AgentLoopStatus::Blocked {
                                reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                    max_model_turns: config.max_model_turns(),
                                },
                            },
                            events,
                            model_turns_run,
                            None,
                            session_usage,
                        ));
                    }

                    if let Some(call) = calls.iter().find_map(|call| match call {
                        PendingLoopToolCall::Bridge(call) => Some(call),
                        PendingLoopToolCall::Runtime(_) | PendingLoopToolCall::FinalOutput(_) => {
                            None
                        }
                    }) {
                        let session_usage = self.usage().await;
                        return Ok(AgentLoopResult::new(
                            AgentLoopStatus::Blocked {
                                reason: AgentLoopBlockedReason::BridgeToolCallRequested {
                                    call_id: call.id().clone(),
                                    tool_name: call.name().clone(),
                                },
                            },
                            events,
                            model_turns_run,
                            None,
                            session_usage,
                        ));
                    }

                    let runtime_calls = calls
                        .into_iter()
                        .map(|call| match call {
                            PendingLoopToolCall::Runtime(call) => call,
                            PendingLoopToolCall::FinalOutput(_) => {
                                unreachable!(
                                    "mixed final-output batches are rejected by provider step"
                                )
                            }
                            PendingLoopToolCall::Bridge(_) => {
                                unreachable!("bridge batches return before runtime execution")
                            }
                        })
                        .collect();
                    let execution = self
                        .execute_tool_call_batch_with_active_permit(
                            runtime_calls,
                            ToolExecutionContext::new(loop_token.clone()),
                            &loop_permit,
                        )
                        .await;
                    let (mut execution_events, error) = execution.into_parts();
                    events.append(&mut execution_events);

                    if let Some(error) = error {
                        if let RuntimeError::ToolExecutionCancelled { call_id, .. } = error {
                            let session_usage = self.usage().await;
                            return Ok(AgentLoopResult::new(
                                AgentLoopStatus::Cancelled {
                                    diagnostic: tool_execution_cancelled_diagnostic(&call_id),
                                },
                                events,
                                model_turns_run,
                                None,
                                session_usage,
                            ));
                        }
                        trace_loop_error(self.session_id().as_str(), model_turns_run, &error);
                        return Err(AgentLoopError::new(events, error));
                    }

                    next_input = Some(continuation_step_input());
                }
                StepOutcome::Pending(PendingLoopToolCall::Runtime(call)) => {
                    if model_turns_run >= config.max_model_turns() {
                        trace_loop_finish(
                            self.session_id().as_str(),
                            "blocked",
                            model_turns_run,
                            Some("max_model_turns_reached"),
                        );
                        let session_usage = self.usage().await;
                        return Ok(AgentLoopResult::new(
                            AgentLoopStatus::Blocked {
                                reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                    max_model_turns: config.max_model_turns(),
                                },
                            },
                            events,
                            model_turns_run,
                            None,
                            session_usage,
                        ));
                    }

                    tracing::info!(
                        event = "runtime.tool.pending",
                        session_id = self.session_id().as_str(),
                        step_index = model_turns_run,
                        tool_call_id = call.id().as_str(),
                        tool_name = call.name().as_str(),
                        "runtime loop saw pending tool"
                    );
                    tracing::info!(
                        event = "runtime.tool.execute.start",
                        session_id = self.session_id().as_str(),
                        step_index = model_turns_run,
                        tool_call_id = call.id().as_str(),
                        tool_name = call.name().as_str(),
                        "runtime loop tool execution start"
                    );
                    match self
                        .execute_tool_call_with_active_permit(
                            call.id(),
                            ToolExecutionContext::new(loop_token.clone()),
                            &loop_permit,
                        )
                        .await
                    {
                        Ok(execution_events) => {
                            if !tool_resolution_is_policy_denied(&execution_events) {
                                tracing::info!(
                                    event = "runtime.tool.execute.finish",
                                    session_id = self.session_id().as_str(),
                                    step_index = model_turns_run,
                                    tool_call_id = call.id().as_str(),
                                    tool_name = call.name().as_str(),
                                    status = tool_resolution_status(&execution_events),
                                    artifact_id = tool_resolution_artifact_id(&execution_events),
                                    diagnostic_code =
                                        tool_resolution_diagnostic_code(&execution_events)
                                            .unwrap_or(""),
                                    "runtime loop tool execution finish"
                                );
                            }
                            events.extend(execution_events);
                        }
                        Err(RuntimeError::ToolExecutionCancelled { call_id, .. }) => {
                            trace_loop_finish(
                                self.session_id().as_str(),
                                "cancelled",
                                model_turns_run,
                                Some("tool_execution_cancelled"),
                            );
                            let session_usage = self.usage().await;
                            return Ok(AgentLoopResult::new(
                                AgentLoopStatus::Cancelled {
                                    diagnostic: tool_execution_cancelled_diagnostic(&call_id),
                                },
                                events,
                                model_turns_run,
                                None,
                                session_usage,
                            ));
                        }
                        Err(source) => {
                            trace_loop_error(self.session_id().as_str(), model_turns_run, &source);
                            return Err(AgentLoopError::new(events, source));
                        }
                    }

                    next_input = Some(continuation_step_input());
                }
            }
        }
    }

    /// Starts a bounded runtime-owned agent loop and returns live events.
    ///
    /// This has the same loop semantics as [`Runtime::run_agent_loop`], but it
    /// yields each observed [`RuntimeJournalEvent`] as soon as the underlying step or
    /// tool execution produces it. Dropping the returned stream cancels the loop
    /// token and aborts the loop producer.
    pub fn run_agent_loop_stream(
        &self,
        input: StepInput,
        context: StepContext,
        config: AgentLoopConfig,
    ) -> Result<AgentLoopEventStream, RuntimeError> {
        let loop_permit = self.acquire_active_step_permit()?;
        let (parent_token, generation_config, _final_output_contract) = context.into_parts();
        let loop_token = parent_token.child_token();
        let producer_token = loop_token.clone();
        let (sender, receiver) = mpsc::channel(16);
        let (result_sender, result_receiver) = oneshot::channel();
        let (bridge_sender, bridge_receiver) = mpsc::channel(1);
        let runtime = self.clone();
        let session_id = self.session_id().clone();
        let producer_handle = tokio::spawn(async move {
            let result = run_agent_loop_stream_producer(AgentLoopStreamProducer {
                runtime,
                input,
                loop_token: producer_token,
                generation_config,
                config,
                loop_permit,
                sender,
                bridge_receiver,
            })
            .await;
            let _ = result_sender.send(result);
        });

        Ok(AgentLoopEventStream::new(
            session_id,
            ReceiverStream::new(receiver),
            loop_token,
            producer_handle,
            result_receiver,
            bridge_sender,
        ))
    }
}

struct AgentLoopStreamProducer {
    runtime: Runtime,
    input: StepInput,
    loop_token: tokio_util::sync::CancellationToken,
    generation_config: merry_llm::GenerationConfig,
    config: AgentLoopConfig,
    loop_permit: ActiveStepPermit,
    sender: mpsc::Sender<AgentLoopStreamMessage>,
    bridge_receiver: mpsc::Receiver<BridgeToolResultCommand>,
}

async fn run_agent_loop_stream_producer(
    producer: AgentLoopStreamProducer,
) -> Option<AgentLoopResult> {
    let AgentLoopStreamProducer {
        runtime,
        input,
        loop_token,
        generation_config,
        config,
        loop_permit,
        sender,
        mut bridge_receiver,
    } = producer;
    let mut next_input = Some(input);
    let mut events = Vec::new();
    let mut projector = RuntimeEventProjector::new();
    let mut model_turns_run = 0;

    while let Some(input) = next_input.take() {
        if loop_token.is_cancelled() {
            return None;
        }

        let mut step_context =
            StepContext::new(loop_token.clone()).with_generation_config(generation_config.clone());
        if let Some(contract) = config.final_output_contract().cloned() {
            step_context = step_context.with_final_output_contract(contract);
        }
        let Ok(stream) = runtime.step_with_active_permit(input, step_context, loop_permit.clone())
        else {
            return None;
        };
        model_turns_run += 1;

        let mut step_events = Vec::new();
        tokio::pin!(stream);
        while let Some(event) = stream.next().await {
            step_events.push(event.clone());
            if !publish_journal_event(&runtime, &mut projector, &sender, &mut events, event).await {
                return None;
            }
        }

        let step_final_output = final_assistant_output_from_step(&runtime, &step_events).await;
        match classify_step_events(&step_events, config.final_output_contract()) {
            StepOutcome::Completed => {
                let session_usage = runtime.usage().await;
                return Some(AgentLoopResult::new(
                    AgentLoopStatus::Completed,
                    events,
                    model_turns_run,
                    step_final_output,
                    session_usage,
                ));
            }
            StepOutcome::Failed(diagnostic) => {
                let session_usage = runtime.usage().await;
                return Some(AgentLoopResult::new(
                    AgentLoopStatus::Failed { diagnostic },
                    events,
                    model_turns_run,
                    None,
                    session_usage,
                ));
            }
            StepOutcome::Cancelled(diagnostic) => {
                let session_usage = runtime.usage().await;
                return Some(AgentLoopResult::new(
                    AgentLoopStatus::Cancelled { diagnostic },
                    events,
                    model_turns_run,
                    None,
                    session_usage,
                ));
            }
            StepOutcome::Blocked(reason) => {
                let session_usage = runtime.usage().await;
                return Some(AgentLoopResult::new(
                    AgentLoopStatus::Blocked { reason },
                    events,
                    model_turns_run,
                    None,
                    session_usage,
                ));
            }
            StepOutcome::ToolResultRecorded => {
                if model_turns_run >= config.max_model_turns() {
                    let session_usage = runtime.usage().await;
                    return Some(AgentLoopResult::new(
                        AgentLoopStatus::Blocked {
                            reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                max_model_turns: config.max_model_turns(),
                            },
                        },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }

                next_input = Some(continuation_step_input());
            }
            StepOutcome::PendingBatch(calls) => {
                if model_turns_run >= config.max_model_turns() {
                    let session_usage = runtime.usage().await;
                    return Some(AgentLoopResult::new(
                        AgentLoopStatus::Blocked {
                            reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                max_model_turns: config.max_model_turns(),
                            },
                        },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }

                let mut runtime_wave = Vec::new();
                for call in calls {
                    match call {
                        PendingLoopToolCall::Runtime(call) => runtime_wave.push(call),
                        PendingLoopToolCall::Bridge(call) => {
                            if let Some(error) = execute_stream_runtime_batch(
                                &runtime,
                                std::mem::take(&mut runtime_wave),
                                &loop_token,
                                &loop_permit,
                                &mut projector,
                                &sender,
                                &mut events,
                            )
                            .await?
                            {
                                if let RuntimeError::ToolExecutionCancelled { call_id, .. } = error
                                {
                                    let session_usage = runtime.usage().await;
                                    return Some(AgentLoopResult::new(
                                        AgentLoopStatus::Cancelled {
                                            diagnostic: tool_execution_cancelled_diagnostic(
                                                &call_id,
                                            ),
                                        },
                                        events,
                                        model_turns_run,
                                        None,
                                        session_usage,
                                    ));
                                }
                                return None;
                            }
                            receive_and_publish_bridge_tool_result(
                                &runtime,
                                call,
                                &loop_token,
                                &loop_permit,
                                &mut bridge_receiver,
                                &mut projector,
                                &sender,
                                &mut events,
                            )
                            .await?;
                        }
                        PendingLoopToolCall::FinalOutput(_) => {
                            unreachable!("mixed final-output batches are rejected by provider step")
                        }
                    }
                }

                if let Some(error) = execute_stream_runtime_batch(
                    &runtime,
                    runtime_wave,
                    &loop_token,
                    &loop_permit,
                    &mut projector,
                    &sender,
                    &mut events,
                )
                .await?
                {
                    if let RuntimeError::ToolExecutionCancelled { call_id, .. } = error {
                        let session_usage = runtime.usage().await;
                        return Some(AgentLoopResult::new(
                            AgentLoopStatus::Cancelled {
                                diagnostic: tool_execution_cancelled_diagnostic(&call_id),
                            },
                            events,
                            model_turns_run,
                            None,
                            session_usage,
                        ));
                    }
                    return None;
                }

                next_input = Some(continuation_step_input());
            }
            StepOutcome::Pending(call) => match call {
                PendingLoopToolCall::FinalOutput(call) => {
                    if let Some(Err(error)) = config
                        .final_output_contract()
                        .map(|contract| contract.validate_call(&call))
                    {
                        let failure_events = runtime
                            .submit_tool_input_validation_failure_with_active_permit(
                                &call,
                                error,
                                &loop_permit,
                            )
                            .await
                            .ok()?;

                        for event in failure_events {
                            if !publish_journal_event(
                                &runtime,
                                &mut projector,
                                &sender,
                                &mut events,
                                event,
                            )
                            .await
                            {
                                return None;
                            }
                        }

                        if model_turns_run >= config.max_model_turns() {
                            let session_usage = runtime.usage().await;
                            return Some(AgentLoopResult::new(
                                AgentLoopStatus::Blocked {
                                    reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                        max_model_turns: config.max_model_turns(),
                                    },
                                },
                                events,
                                model_turns_run,
                                None,
                                session_usage,
                            ));
                        }

                        next_input = Some(continuation_step_input());
                        continue;
                    }

                    let (final_output, events_for_final_output) =
                        record_final_output_tool_call(&runtime, call).await.ok()?;
                    for event in events_for_final_output {
                        if !publish_journal_event(
                            &runtime,
                            &mut projector,
                            &sender,
                            &mut events,
                            event,
                        )
                        .await
                        {
                            return None;
                        }
                    }
                    let session_usage = runtime.usage().await;
                    return Some(AgentLoopResult::new_with_final_output_json(
                        AgentLoopStatus::Completed,
                        events,
                        model_turns_run,
                        None,
                        Some(final_output),
                        session_usage,
                    ));
                }
                call => {
                    if model_turns_run >= config.max_model_turns() {
                        let session_usage = runtime.usage().await;
                        return Some(AgentLoopResult::new(
                            AgentLoopStatus::Blocked {
                                reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                    max_model_turns: config.max_model_turns(),
                                },
                            },
                            events,
                            model_turns_run,
                            None,
                            session_usage,
                        ));
                    }

                    let execution_events = match call {
                        PendingLoopToolCall::Runtime(call) => {
                            match runtime
                                .execute_tool_call_with_active_permit(
                                    call.id(),
                                    ToolExecutionContext::new(loop_token.clone()),
                                    &loop_permit,
                                )
                                .await
                            {
                                Ok(events) => events,
                                Err(RuntimeError::ToolExecutionCancelled { call_id, .. }) => {
                                    let session_usage = runtime.usage().await;
                                    return Some(AgentLoopResult::new(
                                        AgentLoopStatus::Cancelled {
                                            diagnostic: tool_execution_cancelled_diagnostic(
                                                &call_id,
                                            ),
                                        },
                                        events,
                                        model_turns_run,
                                        None,
                                        session_usage,
                                    ));
                                }
                                Err(_) => return None,
                            }
                        }
                        PendingLoopToolCall::Bridge(call) => {
                            let command =
                                receive_bridge_tool_result(&mut bridge_receiver, &loop_token)
                                    .await?;

                            let call_id = command.result.call_id().clone();
                            let result = if call_id == *call.id() {
                                runtime
                                    .submit_tool_result_with_active_permit(
                                        command.result,
                                        command.content,
                                        &loop_permit,
                                    )
                                    .await
                            } else {
                                Err(RuntimeError::UnknownToolCall {
                                    session_id: runtime.session_id().clone(),
                                    call_id,
                                })
                            };

                            match result {
                                Ok(events) => {
                                    let _ = command.ack_sender.send(Ok(()));
                                    events
                                }
                                Err(error) => {
                                    let _ = command.ack_sender.send(Err(error));
                                    return None;
                                }
                            }
                        }
                        PendingLoopToolCall::FinalOutput(_) => {
                            unreachable!("final-output call is handled before continuation budget")
                        }
                    };

                    for event in execution_events {
                        if !publish_journal_event(
                            &runtime,
                            &mut projector,
                            &sender,
                            &mut events,
                            event,
                        )
                        .await
                        {
                            return None;
                        }
                    }

                    next_input = Some(continuation_step_input());
                }
            },
        }
    }

    None
}

fn trace_loop_finish(
    session_id: &str,
    status: &'static str,
    model_turns_run: usize,
    diagnostic_code: Option<&str>,
) {
    tracing::info!(
        event = "runtime.loop.finish",
        session_id,
        status,
        model_turns_run,
        diagnostic_code = diagnostic_code.unwrap_or(""),
        "runtime loop finish"
    );
}

fn trace_loop_error(session_id: &str, model_turns_run: usize, source: &RuntimeError) {
    trace_loop_finish(
        session_id,
        "error",
        model_turns_run,
        Some(runtime_error_code(source)),
    );
}

async fn collect_step_events(stream: RuntimeJournalEventStream) -> Vec<RuntimeJournalEvent> {
    stream.collect().await
}

async fn final_assistant_output_from_step(
    runtime: &Runtime,
    events: &[RuntimeJournalEvent],
) -> Option<String> {
    for event in events.iter().rev() {
        let RuntimeJournalPayload::AssistantOutputRecorded { artifact } = &event.payload else {
            continue;
        };
        if artifact.kind() != &ArtifactKind::Text {
            continue;
        }
        let Ok(content) = runtime.read_artifact_content(artifact.id()).await else {
            continue;
        };
        if let Some(text) = content.as_text() {
            return Some(text.to_owned());
        }
    }

    None
}

async fn record_final_output_tool_call(
    runtime: &Runtime,
    call: PendingToolCall,
) -> Result<(FinalOutput, Vec<RuntimeJournalEvent>), RuntimeError> {
    runtime.record_final_output_tool_call(call).await
}

async fn publish_journal_event(
    runtime: &Runtime,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentLoopStreamMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
    event: RuntimeJournalEvent,
) -> bool {
    let bridge_call = match &event.payload {
        RuntimeJournalPayload::BridgeToolCallRequested { call } => Some(call.clone()),
        _ => None,
    };
    let projected = projector
        .project(event.clone(), runtime)
        .await
        .ok()
        .flatten();

    events.push(event);

    if let Some(projected) = projected
        && sender
            .send(AgentLoopStreamMessage::Event(projected))
            .await
            .is_err()
    {
        return false;
    }

    if let Some(call) = bridge_call
        && sender
            .send(AgentLoopStreamMessage::BridgeToolRequest { call })
            .await
            .is_err()
    {
        return false;
    }

    true
}

fn continuation_step_input() -> StepInput {
    StepInput::no_new_user_input()
}

fn tool_execution_cancelled_diagnostic(call_id: &merry_core::ToolCallId) -> ErrorInfo {
    ErrorInfo::new(
        "tool_execution_cancelled",
        &format!("tool call {call_id} execution was cancelled"),
    )
    .expect("static code and runtime-owned tool call id form a valid diagnostic")
}

fn blocked_reason_code(reason: &AgentLoopBlockedReason) -> &'static str {
    match reason {
        AgentLoopBlockedReason::MaxModelTurnsReached { .. } => "max_model_turns_reached",
        AgentLoopBlockedReason::MultiplePendingToolCalls { .. } => "multiple_pending_tool_calls",
        AgentLoopBlockedReason::StepCompletedWithPendingToolCall { .. } => {
            "step_completed_with_pending_tool_call"
        }
        AgentLoopBlockedReason::StepEndedWithoutTerminalEvent => {
            "step_ended_without_terminal_event"
        }
        AgentLoopBlockedReason::FinalOutputToolNotCalled => "final_output_tool_not_called",
        AgentLoopBlockedReason::BridgeToolCallRequested { .. } => "bridge_tool_call_requested",
    }
}

fn runtime_error_code(error: &RuntimeError) -> &'static str {
    match error {
        RuntimeError::StepAlreadyActive { .. } => "step_already_active",
        RuntimeError::InvalidStepInput { .. } => "invalid_step_input",
        RuntimeError::InvalidUserImageInput { .. } => "invalid_user_image_input",
        RuntimeError::ReservedArtifactId { .. } => "reserved_artifact_id",
        RuntimeError::UnknownToolCall { .. } => "unknown_tool_call",
        RuntimeError::ToolCallAlreadyResolved { .. } => "tool_call_already_resolved",
        RuntimeError::DuplicateToolRegistration { .. } => "duplicate_tool_registration",
        RuntimeError::InvalidToolInputSchema { .. } => "invalid_tool_input_schema",
        RuntimeError::BridgeToolsNotAllowed { .. } => "bridge_tools_not_allowed",
        RuntimeError::ToolExecutionCancelled { .. } => "tool_execution_cancelled",
        RuntimeError::ToolExecutionFailed { .. } => "tool_execution_failed",
        RuntimeError::MissingActionExecutionEvidence { .. } => "missing_action_execution_evidence",
        RuntimeError::MutatingActionCommitLifecycleRequired { .. } => {
            "mutating_action_commit_lifecycle_required"
        }
        RuntimeError::UnsupportedToolResultContent { .. } => "unsupported_tool_result_content",
        RuntimeError::TranscriptItemIdExhausted => "transcript_item_id_exhausted",
        RuntimeError::ModelTurnIdExhausted => "model_turn_id_exhausted",
        RuntimeError::UnknownModelTurn { .. } => "unknown_model_turn",
        RuntimeError::InvalidModelTurnTransition { .. } => "invalid_model_turn_transition",
        RuntimeError::TranscriptToolCallMissing { .. } => "transcript_tool_call_missing",
        RuntimeError::Core { .. } => "core_error",
        RuntimeError::Artifact { .. } => "artifact_error",
        RuntimeError::Context { .. } => "context_error",
        RuntimeError::Checkpoint { .. } => "checkpoint_error",
        RuntimeError::Compaction { .. } => "compaction_error",
        RuntimeError::SessionStore { .. } => "session_store",
        RuntimeError::MissingModelProvider { .. } => "missing_model_provider",
        RuntimeError::CompactionModelRequest { .. } => "compaction_model_request",
        RuntimeError::CompactionModelWindowTooSmall { .. } => "compaction_model_window_too_small",
        RuntimeError::CompactionModelInputTooLarge { .. } => "compaction_model_input_too_large",
        RuntimeError::CompactionModelSetup { .. } => "compaction_model_setup",
        RuntimeError::CompactionModelStream { .. } => "compaction_model_stream",
    }
}

fn tool_resolution_status(events: &[RuntimeJournalEvent]) -> &'static str {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(match result.status() {
                ToolCallResultStatus::Succeeded => "succeeded",
                ToolCallResultStatus::Failed => "failed",
            }),
            _ => None,
        })
        .unwrap_or("unresolved")
}

fn tool_resolution_artifact_id(events: &[RuntimeJournalEvent]) -> String {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => {
                Some(result.artifact().id().as_str().to_owned())
            }
            _ => None,
        })
        .unwrap_or_default()
}

fn tool_resolution_diagnostic_code(events: &[RuntimeJournalEvent]) -> Option<&str> {
    events.iter().find_map(|event| match &event.payload {
        RuntimeJournalPayload::ToolCallResolved { result } => {
            result.diagnostic().map(merry_core::ErrorInfo::code)
        }
        _ => None,
    })
}

fn tool_resolution_is_policy_denied(events: &[RuntimeJournalEvent]) -> bool {
    tool_resolution_diagnostic_code(events) == Some("action_policy_denied")
}

pub(crate) enum StepOutcome {
    Completed,
    Failed(ErrorInfo),
    Cancelled(ErrorInfo),
    ToolResultRecorded,
    Pending(PendingLoopToolCall),
    PendingBatch(Vec<PendingLoopToolCall>),
    Blocked(AgentLoopBlockedReason),
}

#[derive(Clone)]
pub(crate) enum PendingLoopToolCall {
    Runtime(PendingToolCall),
    Bridge(PendingToolCall),
    FinalOutput(PendingToolCall),
}

pub(crate) fn classify_step_events(
    events: &[RuntimeJournalEvent],
    final_output_contract: Option<&FinalOutputContract>,
) -> StepOutcome {
    let bridge_call_ids = events
        .iter()
        .filter_map(|event| match &event.payload {
            RuntimeJournalPayload::BridgeToolCallRequested { call } => Some(call.id().clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();

    let resolved_call_ids = events
        .iter()
        .filter_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result.call_id().clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let resolved_tool_result_recorded = !resolved_call_ids.is_empty();

    let mut pending = events
        .iter()
        .flat_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call }
                if !resolved_call_ids.contains(call.id()) =>
            {
                vec![call.clone()]
            }
            RuntimeJournalPayload::ToolCallBatchPending { batch } => batch
                .calls()
                .iter()
                .filter(|call| !resolved_call_ids.contains(call.id()))
                .cloned()
                .collect(),
            _ => Vec::new(),
        })
        .collect::<Vec<_>>();

    if let Some(diagnostic) = events.iter().rev().find_map(|event| match &event.payload {
        RuntimeJournalPayload::Failed { diagnostic } => Some(diagnostic.clone()),
        _ => None,
    }) {
        return StepOutcome::Failed(diagnostic);
    }

    if let Some(diagnostic) = events.iter().rev().find_map(|event| match &event.payload {
        RuntimeJournalPayload::Cancelled { diagnostic } => Some(diagnostic.clone()),
        _ => None,
    }) {
        return StepOutcome::Cancelled(diagnostic);
    }

    let completed = events
        .iter()
        .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted));

    if completed {
        if pending.is_empty() {
            if final_output_contract.is_some() {
                return StepOutcome::Blocked(AgentLoopBlockedReason::FinalOutputToolNotCalled);
            }
            return StepOutcome::Completed;
        }

        return StepOutcome::Blocked(AgentLoopBlockedReason::StepCompletedWithPendingToolCall {
            pending_count: pending.len(),
        });
    }

    match pending.len() {
        0 if resolved_tool_result_recorded => StepOutcome::ToolResultRecorded,
        0 => StepOutcome::Blocked(AgentLoopBlockedReason::StepEndedWithoutTerminalEvent),
        1 => {
            let call = pending.pop().expect("one pending call is present");
            StepOutcome::Pending(classify_pending_tool_call(
                call,
                &bridge_call_ids,
                final_output_contract,
            ))
        }
        _ => StepOutcome::PendingBatch(
            pending
                .into_iter()
                .map(|call| {
                    classify_pending_tool_call(call, &bridge_call_ids, final_output_contract)
                })
                .collect(),
        ),
    }
}

fn classify_pending_tool_call(
    call: PendingToolCall,
    bridge_call_ids: &BTreeSet<ToolCallId>,
    final_output_contract: Option<&FinalOutputContract>,
) -> PendingLoopToolCall {
    if final_output_contract.is_some_and(|contract| call.name() == contract.tool_name()) {
        PendingLoopToolCall::FinalOutput(call)
    } else if bridge_call_ids.contains(call.id()) {
        PendingLoopToolCall::Bridge(call)
    } else {
        PendingLoopToolCall::Runtime(call)
    }
}

async fn receive_bridge_tool_result(
    receiver: &mut mpsc::Receiver<BridgeToolResultCommand>,
    token: &tokio_util::sync::CancellationToken,
) -> Option<BridgeToolResultCommand> {
    tokio::select! {
        command = receiver.recv() => command,
        () = token.cancelled() => None,
    }
}

async fn execute_stream_runtime_batch(
    runtime: &Runtime,
    calls: Vec<PendingToolCall>,
    token: &tokio_util::sync::CancellationToken,
    loop_permit: &ActiveStepPermit,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentLoopStreamMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
) -> Option<Option<RuntimeError>> {
    if calls.is_empty() {
        return Some(None);
    }

    let execution = runtime
        .execute_tool_call_batch_with_active_permit(
            calls,
            ToolExecutionContext::new(token.clone()),
            loop_permit,
        )
        .await;
    let (execution_events, error) = execution.into_parts();
    for event in execution_events {
        if !publish_journal_event(runtime, projector, sender, events, event).await {
            return None;
        }
    }
    Some(error)
}

#[allow(clippy::too_many_arguments)]
async fn receive_and_publish_bridge_tool_result(
    runtime: &Runtime,
    call: PendingToolCall,
    token: &tokio_util::sync::CancellationToken,
    loop_permit: &ActiveStepPermit,
    receiver: &mut mpsc::Receiver<BridgeToolResultCommand>,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentLoopStreamMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
) -> Option<()> {
    let command = receive_bridge_tool_result(receiver, token).await?;
    let call_id = command.result.call_id().clone();
    let result = if call_id == *call.id() {
        runtime
            .submit_tool_result_with_active_permit(command.result, command.content, loop_permit)
            .await
    } else {
        Err(RuntimeError::UnknownToolCall {
            session_id: runtime.session_id().clone(),
            call_id,
        })
    };

    match result {
        Ok(result_events) => {
            let _ = command.ack_sender.send(Ok(()));
            for event in result_events {
                if !publish_journal_event(runtime, projector, sender, events, event).await {
                    return None;
                }
            }
            Some(())
        }
        Err(error) => {
            let _ = command.ack_sender.send(Err(error));
            None
        }
    }
}
