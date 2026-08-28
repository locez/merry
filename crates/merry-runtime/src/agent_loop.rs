//! Runtime-owned serial agent loop.
//!
//! The MVP loop composes existing runtime primitives:
//! [`Runtime::step`] -> [`Runtime::execute_tool_call`] -> continuation
//! [`Runtime::step`]. It is intentionally serial, provider-neutral, and bounded.

use crate::{
    FinalOutput, FinalOutputContract, Runtime, RuntimeError, RuntimeJournalEventStream,
    StepContext, StepInput, ToolExecutionContext, ToolExecutionOutcome,
    bridge::{
        BridgeToolResultCommand, BridgeToolResultPayload, receive_bridge_tool_result,
        resolve_bridge_tool_result_command,
    },
    events::{ActiveStepPermit, RuntimeEventProjector},
    subagent::completion_notification_text,
};
use futures_util::StreamExt;
use merry_core::{
    ArtifactKind, ErrorInfo, PendingToolCall, PendingToolCallBatch, RuntimeEvent,
    RuntimeJournalEvent, RuntimeJournalPayload, SessionId, SessionUsage, ToolCallBatchId,
    ToolCallId, ToolCallResultStatus, ToolName,
};
use std::{
    collections::BTreeSet,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

/// Generic SDK/runtime default for one top-level agent run.
pub const DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS: usize = 128;

/// Retry policy for application-level structured final-output decoding.
///
/// A retry is another model continuation in the same runtime session. The
/// failed final-output call is recorded as a failed tool result before the
/// continuation is started, so the model receives an actionable failure and
/// the session remains resume-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredOutputRetryPolicy {
    max_retries: usize,
}

impl StructuredOutputRetryPolicy {
    /// Creates a policy with the supplied number of retries after the first
    /// structured-output attempt.
    #[must_use]
    pub const fn new(max_retries: usize) -> Self {
        Self { max_retries }
    }

    /// Returns a policy that does not retry a failed structured output.
    #[must_use]
    pub const fn disabled() -> Self {
        Self::new(0)
    }

    /// Returns the maximum number of retries after the first attempt.
    #[must_use]
    pub const fn max_retries(self) -> usize {
        self.max_retries
    }
}

impl Default for StructuredOutputRetryPolicy {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Configuration for [`Runtime::run_agent_loop`].
///
/// `max_model_turns` bounds the number of model turns started by one loop run.
/// Context compaction may happen within the run, but it does not reset this
/// control-flow and cost budget.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLoopConfig {
    max_model_turns: NonZeroUsize,
    final_output_contract: Option<FinalOutputContract>,
    structured_output_retry_policy: StructuredOutputRetryPolicy,
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
            structured_output_retry_policy: StructuredOutputRetryPolicy::default(),
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

    pub(crate) fn merge_context_final_output_contract(
        mut self,
        context_contract: Option<FinalOutputContract>,
    ) -> Result<Self, AgentLoopConfigError> {
        if context_contract.is_some() && self.final_output_contract.is_some() {
            return Err(AgentLoopConfigError::FinalOutputContractConfiguredTwice);
        }
        if let Some(contract) = context_contract {
            self.final_output_contract = Some(contract);
        }
        Ok(self)
    }

    /// Sets retries for an application-level structured-output decoder.
    #[must_use]
    pub fn with_structured_output_retry_policy(
        mut self,
        policy: StructuredOutputRetryPolicy,
    ) -> Self {
        self.structured_output_retry_policy = policy;
        self
    }

    /// Returns the structured-output retry policy.
    #[must_use]
    pub fn structured_output_retry_policy(&self) -> StructuredOutputRetryPolicy {
        self.structured_output_retry_policy
    }
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            max_model_turns: NonZeroUsize::new(DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS)
                .expect("default agent loop model-turn budget is non-zero"),
            final_output_contract: None,
            structured_output_retry_policy: StructuredOutputRetryPolicy::default(),
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
    /// A single loop received its final-output contract from both public input
    /// paths. Silent precedence would make structured-output behavior depend
    /// on which entry point constructed the loop.
    #[error("agent loop final-output contract was configured more than once")]
    FinalOutputContractConfiguredTwice,
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

/// Runtime-owned single-consumer protocol for one agent run.
///
/// [`Self::next_message`] is the only output path. It yields durable runtime
/// events and explicit host-owned tool batches from the same ordered channel,
/// so a consumer cannot accidentally skip a tool handoff by using an
/// event-only stream. Runtime state owns the active batch and its lifecycle.
pub struct AgentRun {
    session_id: SessionId,
    events: ReceiverStream<AgentRunMessage>,
    loop_token: tokio_util::sync::CancellationToken,
    producer_handle: Option<tokio::task::JoinHandle<()>>,
    result_receiver: Option<oneshot::Receiver<Result<AgentLoopResult, AgentLoopError>>>,
    bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
    pending_tool_invocations: Option<PendingToolCallBatch>,
    bridge_resolution_epoch: Arc<AtomicU64>,
    observed_bridge_resolution_epoch: u64,
}

impl AgentRun {
    fn new(
        session_id: SessionId,
        events: ReceiverStream<AgentRunMessage>,
        loop_token: tokio_util::sync::CancellationToken,
        producer_handle: tokio::task::JoinHandle<()>,
        result_receiver: oneshot::Receiver<Result<AgentLoopResult, AgentLoopError>>,
        bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
        bridge_resolution_epoch: Arc<AtomicU64>,
    ) -> Self {
        let observed_bridge_resolution_epoch = bridge_resolution_epoch.load(Ordering::Acquire);
        Self {
            session_id,
            events,
            loop_token,
            producer_handle: Some(producer_handle),
            result_receiver: Some(result_receiver),
            bridge_sender,
            pending_tool_invocations: None,
            bridge_resolution_epoch,
            observed_bridge_resolution_epoch,
        }
    }

    fn synchronize_bridge_resolution(&mut self) {
        let epoch = self.bridge_resolution_epoch.load(Ordering::Acquire);
        if epoch != self.observed_bridge_resolution_epoch {
            self.pending_tool_invocations = None;
            self.observed_bridge_resolution_epoch = epoch;
        }
    }

    /// Submits a complete batch of host-executed tool outcomes.
    ///
    /// The host may prepare results concurrently, then submit the complete set
    /// in any order. The batch itself does not grant parallel-execution safety;
    /// the runtime validates the complete set and records it in pending-call
    /// order before starting the next model turn. Correctable validation errors
    /// keep the batch pending; a non-correctable recording error is converted
    /// into a failed tool result so the model loop can still recover.
    pub async fn submit_bridge_tool_outcomes(
        &mut self,
        batch_id: &ToolCallBatchId,
        outcomes: Vec<(ToolCallId, ToolExecutionOutcome)>,
    ) -> Result<(), RuntimeError> {
        let Some(expected_batch) = self.pending_tool_invocations.as_ref() else {
            return Err(RuntimeError::NoPendingAgentRunToolInvocations {
                session_id: self.session_id.clone(),
            });
        };
        if expected_batch.id() != batch_id {
            return Err(RuntimeError::BridgeToolResultBatchIdMismatch {
                session_id: self.session_id.clone(),
                expected_batch_id: expected_batch.id().clone(),
                received_batch_id: batch_id.clone(),
            });
        }
        if outcomes.is_empty() {
            return Err(RuntimeError::BridgeToolResultBatchEmpty {
                session_id: self.session_id.clone(),
            });
        }
        let result = self
            .submit_bridge_command(BridgeToolResultPayload::Outcomes {
                batch_id: batch_id.clone(),
                outcomes,
            })
            .await;
        match &result {
            Ok(()) => self.pending_tool_invocations = None,
            Err(error) if error.is_retryable_bridge_tool_result() => {}
            Err(_) => self.pending_tool_invocations = None,
        }
        result
    }

    async fn submit_bridge_command(
        &self,
        payload: BridgeToolResultPayload,
    ) -> Result<(), RuntimeError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        let command = BridgeToolResultCommand {
            payload,
            ack_sender,
        };
        self.bridge_sender
            .send(command)
            .await
            .map_err(|_| RuntimeError::AgentRunClosed {
                session_id: self.session_id.clone(),
                message: "agent run closed before accepting the bridge tool result",
            })?;
        ack_receiver
            .await
            .map_err(|_| RuntimeError::AgentRunClosed {
                session_id: self.session_id.clone(),
                message: "agent run closed before acknowledging the bridge tool result",
            })?
    }

    /// Returns the collected loop result once the run has completed.
    ///
    /// Unconsumed events are drained. An unresolved host-tool batch is reported
    /// as an error and cancellation is requested so the producer cannot remain
    /// blocked waiting for a result that the caller has abandoned.
    pub async fn result(&mut self) -> Result<AgentLoopResult, AgentLoopError> {
        loop {
            match self.next_message().await {
                Ok(Some(AgentRunMessage::Event(_))) => {}
                Ok(Some(AgentRunMessage::ToolInvocations { batch: _ })) => {
                    self.request_cancel();
                    self.pending_tool_invocations = None;
                    return Err(agent_loop_stream_error(
                        &self.session_id,
                        Vec::new(),
                        "agent run result requested before the host tool batch was resolved",
                    ));
                }
                Ok(None) => break,
                Err(source) => {
                    self.request_cancel();
                    self.pending_tool_invocations = None;
                    return Err(AgentLoopError::new(Vec::new(), source));
                }
            }
        }

        let Some(result_receiver) = self.result_receiver.take() else {
            return Err(agent_loop_stream_error(
                &self.session_id,
                Vec::new(),
                "agent loop stream result was already consumed",
            ));
        };
        match result_receiver.await {
            Ok(result) => result,
            Err(_) => Err(agent_loop_stream_error(
                &self.session_id,
                Vec::new(),
                "agent loop stream producer stopped before returning a result",
            )),
        }
    }

    /// Cancels the loop producer and waits until its task has stopped.
    ///
    /// This is the output-boundary cancellation path: once a consumer cannot
    /// accept another event, callers must stop provider/tool work before
    /// settling and persisting the runtime state.
    pub async fn cancel_and_wait(&mut self) {
        self.loop_token.cancel();
        self.pending_tool_invocations = None;
        // A producer may be waiting for capacity while publishing a durable
        // event. Drain the bounded channel so cancellation can reach the
        // runtime checkpoint instead of relying on task abortion.
        while self.events.next().await.is_some() {}
        if let Some(handle) = self.producer_handle.take() {
            let _ = handle.await;
        }
    }

    /// Returns the next runtime-owned run message.
    ///
    /// SDK host adapters use this to execute bridge tool calls without
    /// exposing bridge handoff as a public [`RuntimeEvent`].
    pub async fn next_message(&mut self) -> Result<Option<AgentRunMessage>, RuntimeError> {
        self.synchronize_bridge_resolution();
        if let Some(batch) = self.pending_tool_invocations.as_ref() {
            return Err(RuntimeError::AgentRunToolInvocationsPending {
                session_id: self.session_id.clone(),
                batch_id: batch.id().clone(),
            });
        }
        let Some(message) = self.events.next().await else {
            return Ok(None);
        };
        if let AgentRunMessage::ToolInvocations { batch } = &message {
            if batch.calls().is_empty() {
                return Err(RuntimeError::BridgeToolResultBatchEmpty {
                    session_id: self.session_id.clone(),
                });
            }
            self.pending_tool_invocations = Some(batch.clone());
        }
        Ok(Some(message))
    }

    /// Returns the next runtime-owned run message.
    ///
    /// This Rust convenience alias has the same message-first semantics as
    /// [`Self::next_message`]; it never filters host-tool handoffs.
    pub async fn next(&mut self) -> Result<Option<AgentRunMessage>, RuntimeError> {
        self.next_message().await
    }

    /// Requests cancellation without waiting for the producer task to stop.
    ///
    /// This is intended for synchronous cleanup guards that cannot await, such
    /// as a facade tool-invocation lease being dropped before it is resolved.
    /// Callers that own an async lifecycle should prefer [`Self::cancel_and_wait`]
    /// so producer completion and the terminal result are observed explicitly.
    pub fn request_cancel(&mut self) {
        self.loop_token.cancel();
        self.pending_tool_invocations = None;
    }
}

impl Drop for AgentRun {
    fn drop(&mut self) {
        self.loop_token.cancel();
        if let Some(handle) = self.producer_handle.take() {
            handle.abort();
        }
    }
}

/// Message emitted by an [`AgentRun`].
// Keep the public event inline to avoid an allocation for every streamed event.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentRunMessage {
    /// Public SDK/UI event.
    Event(RuntimeEvent),
    /// Ordered host-owned tool calls from one runtime execution wave.
    ///
    /// Runtime-owned calls from the same model response are executed internally
    /// and are not emitted here. A single host call is represented as a
    /// one-call batch; calls are never combined across model responses or
    /// separate run reads.
    ToolInvocations {
        /// Runtime-owned batch ID and host-owned calls in provider/model order.
        /// Every call must be resolved before the run can request the next
        /// runtime execution wave or model response.
        batch: PendingToolCallBatch,
    },
}

impl AgentRunMessage {
    #[must_use]
    pub fn as_event(&self) -> Option<&RuntimeEvent> {
        match self {
            Self::Event(event) => Some(event),
            Self::ToolInvocations { .. } => None,
        }
    }

    /// Borrows the runtime-owned tool batch when this message carries a host
    /// invocation handoff.
    #[must_use]
    pub fn as_tool_invocations(&self) -> Option<&PendingToolCallBatch> {
        match self {
            Self::ToolInvocations { batch } => Some(batch),
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
/// unknown calls, executor infrastructure failure, or an interrupted stream.
/// Cooperative tool cancellation is represented as
/// [`AgentLoopStatus::Cancelled`]. The already-observed runtime events are
/// preserved for callers.
#[derive(Debug, Error)]
#[error("agent loop stopped on runtime method error: {source}")]
pub struct AgentLoopError {
    events: Vec<RuntimeJournalEvent>,
    #[source]
    source: Box<RuntimeError>,
}

impl AgentLoopError {
    fn new(events: Vec<RuntimeJournalEvent>, source: RuntimeError) -> Self {
        Self {
            events,
            source: Box::new(source),
        }
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
        (self.events, *self.source)
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
        let (loop_token, generation_config, context_contract) = context.into_parts();
        let config = config
            .merge_context_final_output_contract(context_contract)
            .map_err(|source| {
                AgentLoopError::new(Vec::new(), RuntimeError::AgentLoopConfig { source })
            })?;
        let loop_permit = self
            .acquire_active_step_permit()
            .map_err(|source| AgentLoopError::new(Vec::new(), source))?;
        let mut next_input = Some(input);
        let mut deferred_user_input = None;
        let mut events = Vec::new();
        let mut model_turns_run = 0;
        let mut structured_output_retries: usize = 0;

        tracing::info!(
            event = "runtime.loop.start",
            session_id = self.session_id().as_str(),
            max_model_turns = config.max_model_turns(),
            "runtime loop start"
        );

        loop {
            if let Some(notification_input) = take_subagent_notification_input(self).await {
                if next_input
                    .as_ref()
                    .is_some_and(|input| !input.user_messages().is_empty())
                {
                    deferred_user_input = next_input.take();
                } else {
                    let _ = next_input.take();
                }
                next_input = Some(notification_input);
            }
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
                    if let Some(notification_input) = take_subagent_notification_input(self).await {
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
                        next_input = Some(notification_input);
                        continue;
                    }
                    if let Some(deferred_user_input) = deferred_user_input.take() {
                        next_input = Some(deferred_user_input);
                        continue;
                    }
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
                    if let Some((notification_input, notification_text)) =
                        take_subagent_notification(self).await
                    {
                        let mut failure_events = match self
                            .submit_tool_execution_failure_with_active_permit(
                                call.id(),
                                &notification_text,
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
                        next_input = Some(notification_input);
                        continue;
                    }
                    if let Some(Err(error)) = config
                        .final_output_contract()
                        .map(|contract| contract.validate_call(&call))
                    {
                        let error_message = error.message();
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
                        structured_output_retries = structured_output_retries.saturating_add(1);
                        if can_retry_structured_output(
                            &config,
                            structured_output_retries,
                            model_turns_run,
                        ) {
                            next_input = Some(continuation_step_input());
                            continue;
                        }

                        return Ok(structured_output_failure_result(
                            self,
                            events,
                            model_turns_run,
                            error_message,
                        )
                        .await);
                    }

                    if let Some(contract) = config.final_output_contract()
                        && let Err(error_message) = validate_final_output(contract, &call)
                    {
                        let mut failure_events = match self
                            .submit_structured_output_failure_with_active_permit(
                                call.id(),
                                &error_message,
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
                        structured_output_retries = structured_output_retries.saturating_add(1);
                        if can_retry_structured_output(
                            &config,
                            structured_output_retries,
                            model_turns_run,
                        ) {
                            next_input = Some(continuation_step_input());
                            continue;
                        }

                        return Ok(structured_output_failure_result(
                            self,
                            events,
                            model_turns_run,
                            error_message,
                        )
                        .await);
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
    /// returns an [`AgentRun`] handle that yields each observed
    /// [`RuntimeJournalEvent`] as soon as the underlying step or tool execution
    /// produces it. Dropping the handle cancels the loop token and aborts the
    /// loop producer as a final cleanup guard.
    pub fn run_agent_loop_stream(
        &self,
        input: StepInput,
        context: StepContext,
        config: AgentLoopConfig,
    ) -> Result<AgentRun, RuntimeError> {
        let (parent_token, generation_config, context_contract) = context.into_parts();
        let config = config
            .merge_context_final_output_contract(context_contract)
            .map_err(|source| RuntimeError::AgentLoopConfig { source })?;
        let loop_permit = self.acquire_active_step_permit()?;
        let loop_token = parent_token.child_token();
        let producer_token = loop_token.clone();
        let (sender, receiver) = mpsc::channel(16);
        let (result_sender, result_receiver) = oneshot::channel();
        let (bridge_sender, bridge_receiver) = mpsc::channel(1);
        let bridge_resolution_epoch = Arc::new(AtomicU64::new(0));
        let producer_bridge_resolution_epoch = Arc::clone(&bridge_resolution_epoch);
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
                bridge_resolution_epoch: producer_bridge_resolution_epoch,
            })
            .await;
            let _ = result_sender.send(result);
        });

        Ok(AgentRun::new(
            session_id,
            ReceiverStream::new(receiver),
            loop_token,
            producer_handle,
            result_receiver,
            bridge_sender,
            bridge_resolution_epoch,
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
    sender: mpsc::Sender<AgentRunMessage>,
    bridge_receiver: mpsc::Receiver<BridgeToolResultCommand>,
    bridge_resolution_epoch: Arc<AtomicU64>,
}

async fn run_agent_loop_stream_producer(
    producer: AgentLoopStreamProducer,
) -> Result<AgentLoopResult, AgentLoopError> {
    let AgentLoopStreamProducer {
        runtime,
        input,
        loop_token,
        generation_config,
        config,
        loop_permit,
        sender,
        mut bridge_receiver,
        bridge_resolution_epoch,
    } = producer;
    let mut next_input = Some(input);
    let mut deferred_user_input = None;
    let mut events = Vec::new();
    let mut projector = RuntimeEventProjector::new();
    let mut model_turns_run = 0;
    let mut bridge_batch_sequence = 0_u64;
    let mut structured_output_retries: usize = 0;

    loop {
        if let Some(notification_input) = take_subagent_notification_input(&runtime).await {
            if next_input
                .as_ref()
                .is_some_and(|input| !input.user_messages().is_empty())
            {
                deferred_user_input = next_input.take();
            } else {
                let _ = next_input.take();
            }
            next_input = Some(notification_input);
        }
        let Some(input) = next_input.take() else {
            break;
        };
        if loop_token.is_cancelled() {
            let session_usage = runtime.usage().await;
            return Ok(AgentLoopResult::new(
                AgentLoopStatus::Cancelled {
                    diagnostic: agent_loop_cancelled_diagnostic(),
                },
                events,
                model_turns_run,
                None,
                session_usage,
            ));
        }

        let mut step_context =
            StepContext::new(loop_token.clone()).with_generation_config(generation_config.clone());
        if let Some(contract) = config.final_output_contract().cloned() {
            step_context = step_context.with_final_output_contract(contract);
        }
        let stream = runtime
            .step_with_active_permit(input, step_context, loop_permit.clone())
            .map_err(|source| {
                agent_loop_stream_error_with_source(&runtime, model_turns_run, &events, source)
            })?;
        model_turns_run += 1;

        let mut step_events = Vec::new();
        tokio::pin!(stream);
        while let Some(event) = stream.next().await {
            step_events.push(event.clone());
            publish_journal_event(&runtime, &mut projector, &sender, &mut events, event)
                .await
                .map_err(|source| {
                    agent_loop_stream_error_with_source(&runtime, model_turns_run, &events, source)
                })?;
        }

        let step_final_output = final_assistant_output_from_step(&runtime, &step_events).await;
        match classify_step_events(&step_events, config.final_output_contract()) {
            StepOutcome::Completed => {
                if let Some(notification_input) = take_subagent_notification_input(&runtime).await {
                    if model_turns_run >= config.max_model_turns() {
                        let session_usage = runtime.usage().await;
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
                    next_input = Some(notification_input);
                    continue;
                }
                if let Some(deferred_user_input) = deferred_user_input.take() {
                    next_input = Some(deferred_user_input);
                    continue;
                }
                let session_usage = runtime.usage().await;
                return Ok(AgentLoopResult::new(
                    AgentLoopStatus::Completed,
                    events,
                    model_turns_run,
                    step_final_output,
                    session_usage,
                ));
            }
            StepOutcome::Failed(diagnostic) => {
                let session_usage = runtime.usage().await;
                return Ok(AgentLoopResult::new(
                    AgentLoopStatus::Failed { diagnostic },
                    events,
                    model_turns_run,
                    None,
                    session_usage,
                ));
            }
            StepOutcome::Cancelled(diagnostic) => {
                let session_usage = runtime.usage().await;
                return Ok(AgentLoopResult::new(
                    AgentLoopStatus::Cancelled { diagnostic },
                    events,
                    model_turns_run,
                    None,
                    session_usage,
                ));
            }
            StepOutcome::Blocked(reason) => {
                let session_usage = runtime.usage().await;
                return Ok(AgentLoopResult::new(
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
            StepOutcome::PendingBatch(calls) => {
                if model_turns_run >= config.max_model_turns() {
                    let session_usage = runtime.usage().await;
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

                let mut waves = Vec::new();
                for call in calls {
                    match call {
                        PendingLoopToolCall::Runtime(call) => match waves.last_mut() {
                            Some(PendingLoopToolWave::Runtime(wave)) => wave.push(call),
                            _ => waves.push(PendingLoopToolWave::Runtime(vec![call])),
                        },
                        PendingLoopToolCall::Bridge(call) => match waves.last_mut() {
                            Some(PendingLoopToolWave::Bridge(wave)) => wave.push(call),
                            _ => waves.push(PendingLoopToolWave::Bridge(vec![call])),
                        },
                        PendingLoopToolCall::FinalOutput(_) => {
                            unreachable!("mixed final-output batches are rejected by provider step")
                        }
                    }
                }

                for wave in waves {
                    match wave {
                        PendingLoopToolWave::Runtime(calls) => {
                            if let Some(error) = execute_stream_runtime_batch(
                                &runtime,
                                calls,
                                &loop_token,
                                &loop_permit,
                                &mut projector,
                                &sender,
                                &mut events,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })? {
                                if let RuntimeError::ToolExecutionCancelled { call_id, .. } = error
                                {
                                    let session_usage = runtime.usage().await;
                                    return Ok(AgentLoopResult::new(
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
                                return Err(agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    error,
                                ));
                            }
                        }
                        PendingLoopToolWave::Bridge(calls) => {
                            let batch_id = next_agent_run_batch_id(&mut bridge_batch_sequence)
                                .map_err(|source| {
                                    agent_loop_stream_error_with_source(
                                        &runtime,
                                        model_turns_run,
                                        &events,
                                        source,
                                    )
                                })?;
                            match receive_and_publish_bridge_tool_results(
                                &runtime,
                                calls,
                                batch_id,
                                &bridge_resolution_epoch,
                                &loop_token,
                                &loop_permit,
                                &mut bridge_receiver,
                                &mut projector,
                                &sender,
                                &mut events,
                            )
                            .await
                            {
                                Ok(()) => {}
                                Err(RuntimeError::ToolExecutionCancelled { call_id, .. }) => {
                                    let session_usage = runtime.usage().await;
                                    return Ok(AgentLoopResult::new(
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
                                Err(source) => {
                                    return Err(agent_loop_stream_error_with_source(
                                        &runtime,
                                        model_turns_run,
                                        &events,
                                        source,
                                    ));
                                }
                            }
                        }
                    }
                }

                next_input = Some(continuation_step_input());
            }
            StepOutcome::Pending(call) => match call {
                PendingLoopToolCall::FinalOutput(call) => {
                    if let Some((notification_input, notification_text)) =
                        take_subagent_notification(&runtime).await
                    {
                        let failure_events = runtime
                            .submit_tool_execution_failure_with_active_permit(
                                call.id(),
                                &notification_text,
                                &loop_permit,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;
                        for event in failure_events {
                            publish_journal_event(
                                &runtime,
                                &mut projector,
                                &sender,
                                &mut events,
                                event,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;
                        }
                        if model_turns_run >= config.max_model_turns() {
                            let session_usage = runtime.usage().await;
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
                        next_input = Some(notification_input);
                        continue;
                    }
                    if let Some(Err(error)) = config
                        .final_output_contract()
                        .map(|contract| contract.validate_call(&call))
                    {
                        let error_message = error.message();
                        let failure_events = runtime
                            .submit_tool_input_validation_failure_with_active_permit(
                                &call,
                                error,
                                &loop_permit,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;

                        for event in failure_events {
                            publish_journal_event(
                                &runtime,
                                &mut projector,
                                &sender,
                                &mut events,
                                event,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;
                        }
                        structured_output_retries = structured_output_retries.saturating_add(1);
                        if can_retry_structured_output(
                            &config,
                            structured_output_retries,
                            model_turns_run,
                        ) {
                            next_input = Some(continuation_step_input());
                            continue;
                        }

                        return Ok(structured_output_failure_result(
                            &runtime,
                            events,
                            model_turns_run,
                            error_message,
                        )
                        .await);
                    }

                    if let Some(contract) = config.final_output_contract()
                        && let Err(error_message) = validate_final_output(contract, &call)
                    {
                        let failure_events = runtime
                            .submit_structured_output_failure_with_active_permit(
                                call.id(),
                                &error_message,
                                &loop_permit,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;

                        for event in failure_events {
                            publish_journal_event(
                                &runtime,
                                &mut projector,
                                &sender,
                                &mut events,
                                event,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;
                        }
                        structured_output_retries = structured_output_retries.saturating_add(1);
                        if can_retry_structured_output(
                            &config,
                            structured_output_retries,
                            model_turns_run,
                        ) {
                            next_input = Some(continuation_step_input());
                            continue;
                        }

                        return Ok(structured_output_failure_result(
                            &runtime,
                            events,
                            model_turns_run,
                            error_message,
                        )
                        .await);
                    }

                    let (final_output, events_for_final_output) =
                        record_final_output_tool_call(&runtime, call)
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;
                    for event in events_for_final_output {
                        publish_journal_event(
                            &runtime,
                            &mut projector,
                            &sender,
                            &mut events,
                            event,
                        )
                        .await
                        .map_err(|source| {
                            agent_loop_stream_error_with_source(
                                &runtime,
                                model_turns_run,
                                &events,
                                source,
                            )
                        })?;
                    }
                    let session_usage = runtime.usage().await;
                    return Ok(AgentLoopResult::new_with_final_output_json(
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
                                    return Ok(AgentLoopResult::new(
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
                                Err(error) => {
                                    return Err(agent_loop_stream_error_with_source(
                                        &runtime,
                                        model_turns_run,
                                        &events,
                                        error,
                                    ));
                                }
                            }
                        }
                        PendingLoopToolCall::Bridge(call) => {
                            let batch = PendingToolCallBatch::new(
                                next_agent_run_batch_id(&mut bridge_batch_sequence).map_err(
                                    |source| {
                                        agent_loop_stream_error_with_source(
                                            &runtime,
                                            model_turns_run,
                                            &events,
                                            source,
                                        )
                                    },
                                )?,
                                vec![call.clone()],
                            )
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    RuntimeError::from(source),
                                )
                            })?;
                            sender
                                .send(AgentRunMessage::ToolInvocations {
                                    batch: batch.clone(),
                                })
                                .await
                                .map_err(|_| {
                                    agent_loop_stream_error(
                                        runtime.session_id(),
                                        events.clone(),
                                        "bridge invocation receiver closed before the request was delivered",
                                    )
                                })?;
                            loop {
                                let command = match receive_bridge_tool_result(
                                    &mut bridge_receiver,
                                    &loop_token,
                                )
                                .await
                                {
                                    Some(command) => command,
                                    None if loop_token.is_cancelled() => {
                                        let settlement = settle_cancelled_bridge_tool_calls(
                                            &runtime,
                                            batch.calls(),
                                            &loop_permit,
                                            &mut projector,
                                            &sender,
                                            &mut events,
                                        )
                                        .await;
                                        bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
                                        settlement.map_err(|source| {
                                            agent_loop_stream_error_with_source(
                                                &runtime,
                                                model_turns_run,
                                                &events,
                                                source,
                                            )
                                        })?;
                                        let session_usage = runtime.usage().await;
                                        return Ok(AgentLoopResult::new(
                                            AgentLoopStatus::Cancelled {
                                                diagnostic: agent_loop_cancelled_diagnostic(),
                                            },
                                            events,
                                            model_turns_run,
                                            None,
                                            session_usage,
                                        ));
                                    }
                                    None => {
                                        return Err(agent_loop_stream_error(
                                            runtime.session_id(),
                                            events,
                                            "bridge tool result channel closed before the call was resolved",
                                        ));
                                    }
                                };

                                let (ack_sender, result) = resolve_bridge_tool_result_command(
                                    &runtime,
                                    &batch,
                                    command,
                                    &loop_permit,
                                )
                                .await;
                                match result {
                                    Ok(events) => {
                                        bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
                                        let _ = ack_sender.send(Ok(()));
                                        break events;
                                    }
                                    Err(error) if error.is_retryable_bridge_tool_result() => {
                                        let _ = ack_sender.send(Err(error));
                                    }
                                    Err(error) => {
                                        let message = error.to_string();
                                        let _ = ack_sender.send(Err(
                                            RuntimeError::BridgeToolResultRejected {
                                                session_id: runtime.session_id().clone(),
                                                message: message.clone(),
                                            },
                                        ));
                                        settle_failed_bridge_tool_calls(
                                            &runtime,
                                            batch.calls(),
                                            &loop_permit,
                                            &mut projector,
                                            &sender,
                                            &mut events,
                                            &message,
                                        )
                                        .await
                                        .map_err(
                                            |source| {
                                                agent_loop_stream_error_with_source(
                                                    &runtime,
                                                    model_turns_run,
                                                    &events,
                                                    source,
                                                )
                                            },
                                        )?;
                                        break Vec::new();
                                    }
                                }
                            }
                        }
                        PendingLoopToolCall::FinalOutput(_) => {
                            unreachable!("final-output call is handled before continuation budget")
                        }
                    };

                    for event in execution_events {
                        publish_journal_event(
                            &runtime,
                            &mut projector,
                            &sender,
                            &mut events,
                            event,
                        )
                        .await
                        .map_err(|source| {
                            agent_loop_stream_error_with_source(
                                &runtime,
                                model_turns_run,
                                &events,
                                source,
                            )
                        })?;
                    }

                    next_input = Some(continuation_step_input());
                }
            },
        }
    }

    Err(agent_loop_stream_error(
        runtime.session_id(),
        events,
        "agent loop producer ended without a terminal result",
    ))
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

pub(crate) async fn record_final_output_tool_call(
    runtime: &Runtime,
    call: PendingToolCall,
) -> Result<(FinalOutput, Vec<RuntimeJournalEvent>), RuntimeError> {
    runtime.record_final_output_tool_call(call).await
}

pub(crate) fn validate_final_output(
    contract: &FinalOutputContract,
    call: &PendingToolCall,
) -> Result<(), String> {
    let json = serde_json::to_string(call.arguments().as_object())
        .map_err(|source| format!("structured output could not be serialized: {source}"))?;
    contract.validate_output(&json)
}

pub(crate) fn can_retry_structured_output(
    config: &AgentLoopConfig,
    retries: usize,
    model_turns_run: usize,
) -> bool {
    retries <= config.structured_output_retry_policy().max_retries()
        && model_turns_run < config.max_model_turns()
}

async fn structured_output_failure_result(
    runtime: &Runtime,
    events: Vec<RuntimeJournalEvent>,
    model_turns_run: usize,
    message: String,
) -> AgentLoopResult {
    tracing::debug!(
        session_id = runtime.session_id().as_str(),
        model_turns_run,
        error = %message,
        "structured output retry policy exhausted"
    );
    let session_usage = runtime.usage().await;
    AgentLoopResult::new(
        AgentLoopStatus::Failed {
            diagnostic: ErrorInfo::new(
                "structured_output_invalid",
                "structured final output could not be decoded as the requested type",
            )
            .expect("static structured output diagnostic is valid"),
        },
        events,
        model_turns_run,
        None,
        session_usage,
    )
}

async fn publish_journal_event(
    runtime: &Runtime,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
    event: RuntimeJournalEvent,
) -> Result<(), RuntimeError> {
    let projected = projector.project(event.clone(), runtime).await?;

    events.push(event);

    if let Some(projected) = projected
        && sender
            .send(AgentRunMessage::Event(projected))
            .await
            .is_err()
    {
        return Err(RuntimeError::AgentRunClosed {
            session_id: runtime.session_id().clone(),
            message: "public event receiver closed before the event was delivered",
        });
    }

    Ok(())
}

fn continuation_step_input() -> StepInput {
    StepInput::no_new_user_input()
}

async fn take_subagent_notification(runtime: &Runtime) -> Option<(StepInput, String)> {
    let statuses = runtime.take_subagent_completion_notifications().await;
    if statuses.is_empty() {
        return None;
    }
    let text = completion_notification_text(&statuses);
    let input = match StepInput::loop_control_text(&text) {
        Ok(input) => input,
        Err(error) => {
            tracing::warn!(%error, "discarding invalid subagent completion notification");
            return None;
        }
    };
    Some((input, text))
}

async fn take_subagent_notification_input(runtime: &Runtime) -> Option<StepInput> {
    take_subagent_notification(runtime)
        .await
        .map(|(input, _)| input)
}

fn agent_loop_cancelled_diagnostic() -> ErrorInfo {
    ErrorInfo::new(
        "agent_loop_cancelled",
        "agent loop cancellation was requested",
    )
    .expect("static agent loop cancellation diagnostic must be valid")
}

fn agent_loop_stream_error(
    session_id: &SessionId,
    events: Vec<RuntimeJournalEvent>,
    message: &'static str,
) -> AgentLoopError {
    AgentLoopError::new(
        events,
        RuntimeError::AgentRunClosed {
            session_id: session_id.clone(),
            message,
        },
    )
}

fn agent_loop_stream_error_with_source(
    runtime: &Runtime,
    model_turns_run: usize,
    events: &[RuntimeJournalEvent],
    source: RuntimeError,
) -> AgentLoopError {
    trace_loop_error(runtime.session_id().as_str(), model_turns_run, &source);
    AgentLoopError::new(events.to_vec(), source)
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
        RuntimeError::AgentLoopConfig { .. } => "agent_loop_config",
        RuntimeError::ChildRuntimeBuild { .. } => "child_runtime_build",
        RuntimeError::InvalidSubagentSelection { .. } => "invalid_subagent_selection",
        RuntimeError::PlanEffectAttribution { .. } => "plan_effect_attribution_failed",
        RuntimeError::PlanSubagentAttemptInactive { .. } => "plan_subagent_attempt_inactive",
        RuntimeError::InvalidUserImageInput { .. } => "invalid_user_image_input",
        RuntimeError::ReservedArtifactId { .. } => "reserved_artifact_id",
        RuntimeError::UnknownToolCall { .. } => "unknown_tool_call",
        RuntimeError::BridgeToolResultBatchEmpty { .. } => "bridge_tool_result_batch_empty",
        RuntimeError::BridgeToolResultBatchMismatch { .. } => "bridge_tool_result_batch_mismatch",
        RuntimeError::BridgeToolResultBatchIdMismatch { .. } => {
            "bridge_tool_result_batch_id_mismatch"
        }
        RuntimeError::AgentRunToolInvocationsPending { .. } => "agent_run_tool_invocations_pending",
        RuntimeError::NoPendingAgentRunToolInvocations { .. } => {
            "no_pending_agent_run_tool_invocations"
        }
        RuntimeError::ToolCallAlreadyResolved { .. } => "tool_call_already_resolved",
        RuntimeError::DuplicateToolRegistration { .. } => "duplicate_tool_registration",
        RuntimeError::ReservedToolName { .. } => "reserved_tool_name",
        RuntimeError::InvalidToolInputSchema { .. } => "invalid_tool_input_schema",
        RuntimeError::BridgeToolsNotAllowed { .. } => "bridge_tools_not_allowed",
        RuntimeError::ToolExecutionCancelled { .. } => "tool_execution_cancelled",
        RuntimeError::ToolExecutionFailed { .. } => "tool_execution_failed",
        RuntimeError::AgentRunClosed { .. } => "agent_run_closed",
        RuntimeError::BridgeToolResultRejected { .. } => "bridge_tool_result_rejected",
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

enum PendingLoopToolWave {
    Runtime(Vec<PendingToolCall>),
    Bridge(Vec<PendingToolCall>),
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

async fn execute_stream_runtime_batch(
    runtime: &Runtime,
    calls: Vec<PendingToolCall>,
    token: &tokio_util::sync::CancellationToken,
    loop_permit: &ActiveStepPermit,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
) -> Result<Option<RuntimeError>, RuntimeError> {
    if calls.is_empty() {
        return Ok(None);
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
        publish_journal_event(runtime, projector, sender, events, event).await?;
    }
    Ok(error)
}

#[allow(clippy::too_many_arguments)]
async fn receive_and_publish_bridge_tool_results(
    runtime: &Runtime,
    calls: Vec<PendingToolCall>,
    batch_id: ToolCallBatchId,
    bridge_resolution_epoch: &AtomicU64,
    token: &tokio_util::sync::CancellationToken,
    loop_permit: &ActiveStepPermit,
    receiver: &mut mpsc::Receiver<BridgeToolResultCommand>,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
) -> Result<(), RuntimeError> {
    let batch = PendingToolCallBatch::new(batch_id, calls).map_err(RuntimeError::from)?;
    let request = AgentRunMessage::ToolInvocations {
        batch: batch.clone(),
    };
    sender
        .send(request)
        .await
        .map_err(|_| RuntimeError::AgentRunClosed {
            session_id: runtime.session_id().clone(),
            message: "bridge invocation receiver closed before the request was delivered",
        })?;

    loop {
        let command = match receive_bridge_tool_result(receiver, token).await {
            Some(command) => command,
            None if token.is_cancelled() => {
                let Some(first_call) = batch.calls().first() else {
                    return Err(RuntimeError::BridgeToolResultBatchEmpty {
                        session_id: runtime.session_id().clone(),
                    });
                };
                let settlement = settle_cancelled_bridge_tool_calls(
                    runtime,
                    batch.calls(),
                    loop_permit,
                    projector,
                    sender,
                    events,
                )
                .await;
                bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
                settlement?;
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: runtime.session_id().clone(),
                    call_id: first_call.id().clone(),
                });
            }
            None => {
                return Err(RuntimeError::AgentRunClosed {
                    session_id: runtime.session_id().clone(),
                    message: "bridge tool result channel closed before invocations were resolved",
                });
            }
        };
        let (ack_sender, result) =
            resolve_bridge_tool_result_command(runtime, &batch, command, loop_permit).await;
        match result {
            Ok(result_events) => {
                bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
                let _ = ack_sender.send(Ok(()));
                for event in result_events {
                    publish_journal_event(runtime, projector, sender, events, event).await?;
                }
                return Ok(());
            }
            Err(error) if error.is_retryable_bridge_tool_result() => {
                let _ = ack_sender.send(Err(error));
            }
            Err(error) => {
                let message = error.to_string();
                let _ = ack_sender.send(Err(RuntimeError::BridgeToolResultRejected {
                    session_id: runtime.session_id().clone(),
                    message: message.clone(),
                }));
                settle_failed_bridge_tool_calls(
                    runtime,
                    batch.calls(),
                    loop_permit,
                    projector,
                    sender,
                    events,
                    &message,
                )
                .await?;
                return Ok(());
            }
        }
    }
}

async fn settle_cancelled_bridge_tool_calls(
    runtime: &Runtime,
    calls: &[PendingToolCall],
    loop_permit: &ActiveStepPermit,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
) -> Result<(), RuntimeError> {
    let pending_ids = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .map(|call| call.id().clone())
        .collect::<BTreeSet<_>>();

    for call in calls.iter().filter(|call| pending_ids.contains(call.id())) {
        let failure_events = runtime
            .submit_tool_abandoned_failure_with_active_permit(
                call.id(),
                "agent run cancelled before the bridge tool result was submitted",
                loop_permit,
            )
            .await?;
        for event in failure_events {
            publish_journal_event(runtime, projector, sender, events, event).await?;
        }
    }

    Ok(())
}

async fn settle_failed_bridge_tool_calls(
    runtime: &Runtime,
    calls: &[PendingToolCall],
    loop_permit: &ActiveStepPermit,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
    message: &str,
) -> Result<(), RuntimeError> {
    let pending_ids = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .map(|call| call.id().clone())
        .collect::<BTreeSet<_>>();

    for call in calls.iter().filter(|call| pending_ids.contains(call.id())) {
        let failure_events = runtime
            .submit_tool_execution_failure_with_active_permit(call.id(), message, loop_permit)
            .await?;
        for event in failure_events {
            publish_journal_event(runtime, projector, sender, events, event).await?;
        }
    }

    Ok(())
}

fn next_agent_run_batch_id(sequence: &mut u64) -> Result<ToolCallBatchId, RuntimeError> {
    let batch_id =
        ToolCallBatchId::new(&format!("agent-run-batch-{sequence}")).map_err(RuntimeError::from)?;
    *sequence = (*sequence).saturating_add(1);
    Ok(batch_id)
}
