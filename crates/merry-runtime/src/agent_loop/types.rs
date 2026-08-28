//! Results and control states for the runtime-owned agent loop.

use crate::{FinalOutput, RuntimeError};
use merry_core::{ErrorInfo, RuntimeJournalEvent, SessionUsage, ToolCallId, ToolName};
use thiserror::Error;

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
    pub(crate) fn new(
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

    pub(crate) fn new_with_final_output_json(
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
    pub(crate) fn new(events: Vec<RuntimeJournalEvent>, source: RuntimeError) -> Self {
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
