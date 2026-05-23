//! Runtime-owned serial agent loop.
//!
//! The MVP loop composes existing runtime primitives:
//! [`Runtime::step`] -> [`Runtime::execute_tool_call`] -> continuation
//! [`Runtime::step`]. It is intentionally serial, provider-neutral, and bounded.

use crate::{
    Runtime, RuntimeError, RuntimeEventStream, StepContext, StepInput, ToolExecutionContext,
};
use futures_util::StreamExt;
use merry_core::{ErrorInfo, PendingToolCall, RuntimeEvent, RuntimeEventKind};
use std::num::NonZeroUsize;
use thiserror::Error;

const DEFAULT_AGENT_LOOP_MAX_STEPS: usize = 16;

/// Fixed user input used for provider continuation steps after a tool result.
///
/// Tool call and result details are compiled from runtime-owned continuation
/// state; this text is only a small prompt nudge for the model turn.
pub const DEFAULT_AGENT_LOOP_CONTINUATION_INPUT: &str = "Continue after tool result.";

const ORIGINAL_TASK_CONTINUATION_LABEL: &str = "Original task:";

/// Configuration for [`Runtime::run_agent_loop`].
///
/// `max_steps` bounds the number of [`Runtime::step`] calls made by one loop
/// run. Tool execution is serial and only happens when budget remains for the
/// following continuation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentLoopConfig {
    max_steps: NonZeroUsize,
}

impl AgentLoopConfig {
    /// Creates loop configuration with a non-zero step budget.
    pub fn new(max_steps: usize) -> Result<Self, AgentLoopConfigError> {
        let Some(max_steps) = NonZeroUsize::new(max_steps) else {
            return Err(AgentLoopConfigError::MaxStepsMustBeNonZero);
        };

        Ok(Self { max_steps })
    }

    /// Maximum number of runtime steps this loop may start.
    #[must_use]
    pub fn max_steps(&self) -> usize {
        self.max_steps.get()
    }
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            max_steps: NonZeroUsize::new(DEFAULT_AGENT_LOOP_MAX_STEPS)
                .expect("default agent loop step budget is non-zero"),
        }
    }
}

/// Invalid agent loop configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AgentLoopConfigError {
    /// A loop without a step budget would either do no useful work or hide a
    /// caller configuration mistake.
    #[error("agent loop max_steps must be greater than zero")]
    MaxStepsMustBeNonZero,
}

/// Result of a completed or policy-blocked agent loop run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLoopResult {
    status: AgentLoopStatus,
    events: Vec<RuntimeEvent>,
    steps_run: usize,
}

impl AgentLoopResult {
    fn new(status: AgentLoopStatus, events: Vec<RuntimeEvent>, steps_run: usize) -> Self {
        Self {
            status,
            events,
            steps_run,
        }
    }

    /// Final loop status.
    #[must_use]
    pub fn status(&self) -> &AgentLoopStatus {
        &self.status
    }

    /// Runtime events collected in emission order.
    #[must_use]
    pub fn events(&self) -> &[RuntimeEvent] {
        &self.events
    }

    /// Number of [`Runtime::step`] calls started by the loop.
    #[must_use]
    pub fn steps_run(&self) -> usize {
        self.steps_run
    }

    /// Consumes the result and returns the collected events.
    #[must_use]
    pub fn into_events(self) -> Vec<RuntimeEvent> {
        self.events
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
    /// The configured runtime-step budget has been reached.
    MaxStepsReached { max_steps: usize },
    /// A step emitted more than one pending tool call. The MVP loop is serial.
    MultiplePendingToolCalls { pending_count: usize },
    /// A step emitted both completion and pending tool-call state.
    StepCompletedWithPendingToolCall { pending_count: usize },
    /// A step stream ended without a completion, failure, cancellation, or
    /// pending tool-call event.
    StepEndedWithoutTerminalEvent,
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
    events: Vec<RuntimeEvent>,
    #[source]
    source: RuntimeError,
}

impl AgentLoopError {
    fn new(events: Vec<RuntimeEvent>, source: RuntimeError) -> Self {
        Self { events, source }
    }

    /// Runtime events collected before the method error.
    #[must_use]
    pub fn events(&self) -> &[RuntimeEvent] {
        &self.events
    }

    /// Underlying runtime method error.
    #[must_use]
    pub fn runtime_error(&self) -> &RuntimeError {
        &self.source
    }

    /// Consumes the error into its preserved events and runtime error.
    #[must_use]
    pub fn into_parts(self) -> (Vec<RuntimeEvent>, RuntimeError) {
        (self.events, self.source)
    }
}

impl Runtime {
    /// Runs a bounded, serial runtime-owned agent loop.
    ///
    /// The loop starts with one [`Runtime::step`]. If the step completes, fails,
    /// or is cancelled, the corresponding status is returned with all observed
    /// events. If the step records exactly one pending tool call and more step
    /// budget remains, the loop executes that call through
    /// [`Runtime::execute_tool_call`], appends its events, and starts a
    /// continuation step using [`DEFAULT_AGENT_LOOP_CONTINUATION_INPUT`].
    ///
    /// The MVP loop does not support parallel tool calls and does not introduce
    /// provider conversation state. It owns the runtime active-step permit for
    /// the full step -> tool execution -> continuation sequence. While the loop
    /// is running, cloned runtime handles reject concurrent direct mutation
    /// APIs with [`RuntimeError::StepAlreadyActive`]. Cancellation and
    /// generation controls are reused from `context` for every step and tool
    /// execution.
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
        let original_task = input.text().to_owned();
        let mut next_input = Some(input);
        let mut events = Vec::new();
        let mut steps_run = 0;

        loop {
            let input = next_input
                .take()
                .expect("agent loop always installs the next step input before continuing");
            let step_context = StepContext::new(loop_token.clone())
                .with_generation_config(generation_config.clone());
            let stream =
                match self.step_with_active_permit(input, step_context, loop_permit.clone()) {
                    Ok(stream) => stream,
                    Err(source) => return Err(AgentLoopError::new(events, source)),
                };
            steps_run += 1;

            let mut step_events = collect_step_events(stream).await;
            let outcome = classify_step_events(&step_events);
            events.append(&mut step_events);

            match outcome {
                StepOutcome::Completed => {
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Completed,
                        events,
                        steps_run,
                    ));
                }
                StepOutcome::Failed(diagnostic) => {
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Failed { diagnostic },
                        events,
                        steps_run,
                    ));
                }
                StepOutcome::Cancelled(diagnostic) => {
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Cancelled { diagnostic },
                        events,
                        steps_run,
                    ));
                }
                StepOutcome::Blocked(reason) => {
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Blocked { reason },
                        events,
                        steps_run,
                    ));
                }
                StepOutcome::Pending(call) => {
                    if steps_run >= config.max_steps() {
                        return Ok(AgentLoopResult::new(
                            AgentLoopStatus::Blocked {
                                reason: AgentLoopBlockedReason::MaxStepsReached {
                                    max_steps: config.max_steps(),
                                },
                            },
                            events,
                            steps_run,
                        ));
                    }

                    match self
                        .execute_tool_call_with_active_permit(
                            call.id(),
                            ToolExecutionContext::new(loop_token.clone()),
                            &loop_permit,
                        )
                        .await
                    {
                        Ok(execution_events) => events.extend(execution_events),
                        Err(RuntimeError::ToolExecutionCancelled { call_id, .. }) => {
                            return Ok(AgentLoopResult::new(
                                AgentLoopStatus::Cancelled {
                                    diagnostic: tool_execution_cancelled_diagnostic(&call_id),
                                },
                                events,
                                steps_run,
                            ));
                        }
                        Err(source) => return Err(AgentLoopError::new(events, source)),
                    }

                    next_input = Some(match continuation_step_input(&original_task) {
                        Ok(input) => input,
                        Err(source) => return Err(AgentLoopError::new(events, source)),
                    });
                }
            }
        }
    }
}

async fn collect_step_events(stream: RuntimeEventStream) -> Vec<RuntimeEvent> {
    stream.collect().await
}

fn continuation_step_input(original_task: &str) -> Result<StepInput, RuntimeError> {
    StepInput::user_text(&format!(
        "{DEFAULT_AGENT_LOOP_CONTINUATION_INPUT}\n\n{ORIGINAL_TASK_CONTINUATION_LABEL}\n{original_task}"
    ))
}

fn tool_execution_cancelled_diagnostic(call_id: &merry_core::ToolCallId) -> ErrorInfo {
    ErrorInfo::new(
        "tool_execution_cancelled",
        &format!("tool call {call_id} execution was cancelled"),
    )
    .expect("static code and runtime-owned tool call id form a valid diagnostic")
}

enum StepOutcome {
    Completed,
    Failed(ErrorInfo),
    Cancelled(ErrorInfo),
    Pending(PendingToolCall),
    Blocked(AgentLoopBlockedReason),
}

fn classify_step_events(events: &[RuntimeEvent]) -> StepOutcome {
    let mut pending = events
        .iter()
        .filter_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => Some(call.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    if let Some(diagnostic) = events.iter().rev().find_map(|event| match &event.kind {
        RuntimeEventKind::Failed { diagnostic } => Some(diagnostic.clone()),
        _ => None,
    }) {
        return StepOutcome::Failed(diagnostic);
    }

    if let Some(diagnostic) = events.iter().rev().find_map(|event| match &event.kind {
        RuntimeEventKind::Cancelled { diagnostic } => Some(diagnostic.clone()),
        _ => None,
    }) {
        return StepOutcome::Cancelled(diagnostic);
    }

    let completed = events
        .iter()
        .any(|event| matches!(event.kind, RuntimeEventKind::StepCompleted));

    if completed {
        if pending.is_empty() {
            return StepOutcome::Completed;
        }

        return StepOutcome::Blocked(AgentLoopBlockedReason::StepCompletedWithPendingToolCall {
            pending_count: pending.len(),
        });
    }

    match pending.len() {
        0 => StepOutcome::Blocked(AgentLoopBlockedReason::StepEndedWithoutTerminalEvent),
        1 => StepOutcome::Pending(pending.pop().expect("one pending call is present")),
        count => StepOutcome::Blocked(AgentLoopBlockedReason::MultiplePendingToolCalls {
            pending_count: count,
        }),
    }
}
