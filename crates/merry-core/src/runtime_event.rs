//! Public runtime event contract for SDK/UI consumers.

use crate::{
    CoordinatorDirectiveSnapshot, ErrorInfo, PendingToolCall, PendingToolCallBatch, PlanAttemptId,
    PlanAttemptProgressSnapshot, PlanAttemptSnapshot, PlanId, PlanLeaseSnapshot, PlanNodeId,
    PlanPhase, PlanRevisionSummary, PlanSnapshot, SessionId, SessionUsage, SubagentId,
    SubagentTaskId, ToolCallId, ToolCallResult,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// SDK-facing runtime event.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeEvent {
    /// A session was initialized.
    SessionStarted { source: RuntimeEventSource },
    /// A runtime step started.
    StepStarted { source: RuntimeEventSource },
    /// A runtime step completed.
    StepCompleted { source: RuntimeEventSource },
    /// Automatic context compaction started.
    CompactionStarted { source: RuntimeEventSource },
    /// Automatic context compaction installed a compacted checkpoint.
    CompactionCompleted {
        checkpoint_id: String,
        covered_history_item_count: usize,
        source: RuntimeEventSource,
    },
    /// Session usage was updated from a provider-reported model response.
    UsageUpdated {
        usage: SessionUsage,
        source: RuntimeEventSource,
    },
    /// Live or usage-corrected throughput; None clears the active observation.
    ModelOutputRateUpdated {
        rate: Option<crate::ModelOutputRate>,
        source: RuntimeEventSource,
    },
    /// The assistant produced user-facing text.
    AssistantMessage {
        text: String,
        source: RuntimeEventSource,
    },
    /// Incremental assistant text from the active model stream.
    ///
    /// Consumers should treat this as progress and use the following
    /// [`RuntimeEvent::AssistantMessage`] as the durable completion event.
    AssistantMessageDelta {
        delta: String,
        source: RuntimeEventSource,
    },
    /// Incremental model reasoning shown while a turn is thinking.
    ///
    /// This is live progress, like [`RuntimeEvent::AssistantMessageDelta`]. The
    /// following assistant message, tool call, or step completion closes the
    /// thinking block.
    ReasoningDelta {
        delta: String,
        source: RuntimeEventSource,
    },
    /// The model requested a tool call.
    ToolCallStarted {
        call: PendingToolCall,
        source: RuntimeEventSource,
    },
    /// The model requested multiple ordered tool calls in one turn.
    ToolCallBatchStarted {
        batch: PendingToolCallBatch,
        source: RuntimeEventSource,
    },
    /// A tool call finished with a recorded result.
    ToolCallFinished {
        result: ToolCallResult,
        output: Option<ToolOutput>,
        source: RuntimeEventSource,
    },
    /// Incremental live output chunk from an active tool call (e.g. `run_process`).
    ///
    /// Live progress only; durable tool output is recorded through
    /// [`RuntimeEvent::ToolCallFinished`].
    ToolCallOutputDelta {
        call_id: ToolCallId,
        stream: ToolOutputStream,
        delta: String,
        source: RuntimeEventSource,
    },
    /// A final-output contract recorded structured terminal output.
    FinalOutputRecorded {
        call_id: ToolCallId,
        json: String,
        source: RuntimeEventSource,
    },
    /// A model provider attempt started.
    ModelRetryAttemptStarted {
        attempt: usize,
        max_attempts: usize,
        source: RuntimeEventSource,
    },
    /// A retryable model provider failure scheduled another attempt.
    ModelRetryScheduled {
        attempt: usize,
        next_attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        error_kind: String,
        source: RuntimeEventSource,
    },
    /// A retryable model provider failure exhausted retry budget.
    ModelRetryExhausted {
        attempts_run: usize,
        max_attempts: usize,
        error_kind: String,
        source: RuntimeEventSource,
    },
    /// A model used a skill by reading its catalog-listed `SKILL.md`.
    SkillUsed {
        skill_name: String,
        skill_md_path: String,
        tool_call_id: ToolCallId,
        source: RuntimeEventSource,
    },
    /// A subagent task was accepted for execution.
    SubagentSpawned {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        task_anchor: String,
        source: RuntimeEventSource,
    },
    /// A subagent started executing.
    SubagentStarted {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        source: RuntimeEventSource,
    },
    /// A subagent reported a lifecycle status update.
    SubagentStatusChanged {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        status: SubagentStatus,
        source: RuntimeEventSource,
    },
    /// A subagent completed and reported compact references to its work.
    SubagentCompleted {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        summary: String,
        output_paths: Vec<String>,
        changed_paths: Vec<String>,
        source: RuntimeEventSource,
    },
    /// A subagent failed.
    SubagentFailed {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        diagnostic: ErrorInfo,
        source: RuntimeEventSource,
    },
    /// A subagent was cancelled.
    SubagentCancelled {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        diagnostic: ErrorInfo,
        source: RuntimeEventSource,
    },
    /// A complete bounded plan snapshot was durably committed.
    PlanUpdated {
        snapshot: PlanSnapshot,
        summary: PlanRevisionSummary,
        source: RuntimeEventSource,
    },
    /// The active plan changed lifecycle phase.
    PlanPhaseChanged {
        plan_id: PlanId,
        phase: PlanPhase,
        source: RuntimeEventSource,
    },
    /// A plan node became dependency- and policy-ready.
    PlanNodeReady {
        plan_id: PlanId,
        node_id: PlanNodeId,
        node_revision: u64,
        source: RuntimeEventSource,
    },
    /// The scheduler durably reserved an execution lease.
    PlanLeaseStarted {
        lease: PlanLeaseSnapshot,
        source: RuntimeEventSource,
    },
    /// Runtime-recorded liveness or activity progress changed.
    PlanProgressUpdated {
        progress: PlanAttemptProgressSnapshot,
        source: RuntimeEventSource,
    },
    /// Runtime policy requested semantic coordinator review of progress.
    PlanProgressReviewRequested {
        plan_id: PlanId,
        attempt_id: PlanAttemptId,
        reason: String,
        source: RuntimeEventSource,
    },
    /// A subagent or local executor durably reported semantic progress.
    PlanAttemptProgressReported {
        progress: PlanAttemptProgressSnapshot,
        source: RuntimeEventSource,
    },
    /// A coordinator directive changed persisted lifecycle state.
    PlanDirectiveUpdated {
        directive: CoordinatorDirectiveSnapshot,
        source: RuntimeEventSource,
    },
    /// One plan attempt reached a typed terminal outcome.
    PlanAttemptFinished {
        attempt: PlanAttemptSnapshot,
        source: RuntimeEventSource,
    },
    /// The runtime failed.
    RunFailed {
        diagnostic: ErrorInfo,
        source: RuntimeEventSource,
    },
    /// The runtime was cancelled.
    RunCancelled {
        diagnostic: ErrorInfo,
        source: RuntimeEventSource,
    },
    /// Interactive driver state changed.
    InteractiveRunStateChanged { state: InteractiveRunState },
    /// Queued pending inputs became provider-visible together as one turn.
    QueuedInputAccepted { inputs: Vec<QueuedInputView> },
    /// The read-only pending-input queue or its pause state changed.
    QueuedInputsChanged { inputs: QueuedInputsView },
    /// A public runtime stream closed.
    Closed,
}

impl RuntimeEvent {
    /// Whether this live-only observation is excluded from retained run results.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::ModelOutputRateUpdated { .. }
                | Self::AssistantMessageDelta { .. }
                | Self::ReasoningDelta { .. }
                | Self::ToolCallOutputDelta { .. }
        )
    }
}

/// Pointer from a public event back to the journal position that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEventSource {
    pub session_id: SessionId,
    pub sequence: u64,
}

impl RuntimeEventSource {
    #[must_use]
    pub fn new(session_id: SessionId, sequence: u64) -> Self {
        Self {
            session_id,
            sequence,
        }
    }
}

/// Compact lifecycle status for subagents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Output stream carrying a live [`RuntimeEvent::ToolCallOutputDelta`] chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputStream {
    Stdout,
    Stderr,
}

/// Kind of recorded tool output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputKind {
    /// UTF-8 plain text.
    Text,
    /// Serialized JSON text.
    Json,
}

/// Complete public text or JSON output for a tool result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolOutput {
    Text { text: String },
    Json { json: String },
}

impl ToolOutput {
    /// Creates UTF-8 text tool output.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Creates serialized JSON tool output.
    #[must_use]
    pub fn json(json: impl Into<String>) -> Self {
        Self::Json { json: json.into() }
    }

    /// Returns the output kind.
    #[must_use]
    pub const fn kind(&self) -> ToolOutputKind {
        match self {
            Self::Text { .. } => ToolOutputKind::Text,
            Self::Json { .. } => ToolOutputKind::Json,
        }
    }

    /// Borrows the exact UTF-8 payload.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text { text } | Self::Json { json: text } => text,
        }
    }

    /// Returns the exact UTF-8 byte length.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.as_str().len()
    }

    /// Creates a bounded UTF-8-safe preview without cloning the full payload.
    #[must_use]
    pub fn bounded_preview(&self, max_bytes: usize) -> ToolOutputPreview {
        let value = self.as_str();
        let byte_length = value.len();
        if byte_length <= max_bytes {
            return ToolOutputPreview::new(self.kind(), value.to_owned(), false, byte_length);
        }
        let mut end = 0;
        for (index, character) in value.char_indices() {
            let next = index + character.len_utf8();
            if next > max_bytes {
                break;
            }
            end = next;
        }
        ToolOutputPreview::new(self.kind(), value[..end].to_owned(), true, byte_length)
    }
}

/// Bounded UTF-8 preview of a recorded tool output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolOutputPreview {
    kind: ToolOutputKind,
    content: String,
    truncated: bool,
    byte_length: usize,
}

impl ToolOutputPreview {
    /// Creates a bounded tool output preview.
    #[must_use]
    pub fn new(kind: ToolOutputKind, content: String, truncated: bool, byte_length: usize) -> Self {
        Self {
            kind,
            content,
            truncated,
            byte_length,
        }
    }

    /// Returns the tool output kind.
    #[must_use]
    pub const fn kind(&self) -> ToolOutputKind {
        self.kind
    }

    /// Borrows the bounded UTF-8 preview content.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns whether the exact output exceeded the preview bound.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// Returns the exact byte length of the full recorded output.
    #[must_use]
    pub const fn byte_length(&self) -> usize {
        self.byte_length
    }
}

/// Lightweight public summary of an attached image without raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageSummary {
    label: String,
    width: u32,
    height: u32,
}

impl ImageSummary {
    /// Creates an image summary.
    #[must_use]
    pub fn new(label: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            label: label.into(),
            width,
            height,
        }
    }

    /// Borrows the human-readable image label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns decoded image width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns decoded image height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }
}

/// Public interactive driver state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InteractiveRunState {
    WaitingForInput,
    RunningModel,
    RunningTool,
    Interrupting,
    Closed,
}

/// Read-only public view of one pending queued input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueuedInputView {
    /// Stable runtime-assigned identifier for targeted edit or removal.
    pub id: u64,
    /// Message text waiting to be sent.
    pub text: String,
    /// Zero-based position in the pending queue.
    pub position: usize,
}

/// Read-only public view of the single pending-input queue.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueuedInputsView {
    /// Pending inputs waiting to be dispatched together at the next boundary.
    pub items: Vec<QueuedInputView>,
    /// Whether automatic dispatch is paused (e.g., while editing a pending item
    /// or after an interrupt).
    pub paused: bool,
}
