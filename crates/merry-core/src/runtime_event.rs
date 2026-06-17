//! Public runtime event contract for SDK/UI consumers.

use crate::{
    ArtifactRef, ErrorInfo, EvidenceRef, PendingToolCall, SessionId, SessionUsage, SubagentId,
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
    /// The assistant produced user-facing text.
    AssistantMessage {
        text: String,
        artifact: ArtifactRef,
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
    /// The model requested a tool call.
    ToolCallStarted {
        call: PendingToolCall,
        source: RuntimeEventSource,
    },
    /// A tool call finished with an artifact-backed result.
    ToolCallFinished {
        result: ToolCallResult,
        output: Option<ToolOutput>,
        source: RuntimeEventSource,
    },
    /// A final-output contract recorded structured terminal output.
    FinalOutputRecorded {
        call_id: ToolCallId,
        artifact: ArtifactRef,
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
    /// Exact evidence was referenced.
    EvidenceReferenced {
        evidence: EvidenceRef,
        source: RuntimeEventSource,
    },
    /// A model used a skill by reading its catalog-listed `SKILL.md`.
    SkillUsed {
        skill_name: String,
        skill_md_path: String,
        tool_call_id: ToolCallId,
        artifact: ArtifactRef,
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
    /// Queued input became provider-visible.
    QueuedInputAccepted {
        lane: QueuedInputLane,
        inputs: Vec<QueuedInputView>,
    },
    /// The read-only queued input view changed.
    QueuedInputsChanged { inputs: QueuedInputsView },
    /// A public runtime stream closed.
    Closed,
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

/// Complete public text or JSON output for a tool result artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolOutput {
    Text { text: String },
    Json { json: String },
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

/// Pending input lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueuedInputLane {
    Next,
    Suspended,
    Backlog,
}

/// Read-only public view of one pending queued input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueuedInputView {
    pub text: String,
    pub lane: QueuedInputLane,
    pub position: usize,
}

/// Read-only public view of all pending queued inputs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueuedInputsView {
    pub next: Vec<QueuedInputView>,
    pub suspended: Vec<QueuedInputView>,
    pub backlog: Vec<QueuedInputView>,
}
