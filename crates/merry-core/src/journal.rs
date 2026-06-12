//! Internal ordered runtime journal contract.

use crate::{
    ArtifactRef, ErrorInfo, EvidenceRef, PendingToolCall, SessionId, SessionUsage, SubagentId,
    SubagentTaskId, ToolCallId, ToolCallResult,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Ordered runtime journal event.
///
/// Journal events are the runtime's durable execution log. They are suitable
/// for diagnostics, replay inspection, and runtime control, but they are not
/// the default SDK/UI event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeJournalEvent {
    /// Session that emitted the event.
    pub session_id: SessionId,
    /// Monotonic event sequence within the session.
    pub sequence: u64,
    /// Event payload.
    pub payload: RuntimeJournalPayload,
}

impl RuntimeJournalEvent {
    /// Creates a runtime journal event.
    #[must_use]
    pub fn new(session_id: SessionId, sequence: u64, payload: RuntimeJournalPayload) -> Self {
        Self {
            session_id,
            sequence,
            payload,
        }
    }
}

/// Ordered runtime journal payload.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeJournalPayload {
    /// A session was initialized.
    SessionStarted,
    /// A runtime step started.
    StepStarted,
    /// A model provider attempt started.
    ModelRetryAttemptStarted {
        /// 1-based attempt number.
        attempt: usize,
        /// Maximum attempts configured for this model turn.
        max_attempts: usize,
    },
    /// A retryable model provider failure scheduled another attempt.
    ModelRetryScheduled {
        /// 1-based failed attempt number.
        attempt: usize,
        /// 1-based next attempt number.
        next_attempt: usize,
        /// Maximum attempts configured for this model turn.
        max_attempts: usize,
        /// Delay before the next attempt, in milliseconds.
        delay_ms: u64,
        /// Provider-neutral error category.
        error_kind: String,
    },
    /// A retryable model provider failure exhausted retry budget.
    ModelRetryExhausted {
        /// Number of attempts that ran.
        attempts_run: usize,
        /// Maximum attempts configured for this model turn.
        max_attempts: usize,
        /// Provider-neutral error category.
        error_kind: String,
    },
    /// A runtime step completed.
    StepCompleted,
    /// Automatic context compaction started.
    CompactionStarted,
    /// Automatic context compaction installed a compacted checkpoint.
    CompactionCompleted {
        /// Installed checkpoint id.
        checkpoint_id: String,
        /// Number of history items covered by the new checkpoint.
        covered_history_item_count: usize,
    },
    /// Session usage was updated from a provider-reported model response.
    SessionUsageUpdated { usage: SessionUsage },
    /// An artifact reference was recorded.
    ArtifactRecorded { artifact: ArtifactRef },
    /// An assistant text output artifact was recorded.
    AssistantOutputRecorded { artifact: ArtifactRef },
    /// Exact evidence was referenced.
    EvidenceReferenced { evidence: EvidenceRef },
    /// A model requested a tool call that is waiting for runtime policy/execution.
    ToolCallPending { call: PendingToolCall },
    /// A model requested a bridge tool call that must be executed by an external runner.
    BridgeToolCallRequested { call: PendingToolCall },
    /// A pending tool call was resolved with an artifact-backed result.
    ToolCallResolved { result: ToolCallResult },
    /// A runtime-owned final-output tool call recorded structured terminal output.
    FinalOutputRecorded {
        /// Provider-originated call id for the final-output tool call.
        call_id: ToolCallId,
        /// JSON artifact containing the final structured output.
        artifact: ArtifactRef,
    },
    /// A model used a skill by successfully reading its catalog-listed `SKILL.md`.
    SkillUsed {
        /// Model-visible skill name from `SKILL.md` frontmatter.
        skill_name: String,
        /// Catalog-listed workspace-readable path to the skill body.
        skill_md_path: String,
        /// Tool call that read the skill body.
        tool_call_id: ToolCallId,
        /// Artifact that contains the read result.
        artifact: ArtifactRef,
    },
    /// A subagent task was accepted for execution.
    SubagentSpawned {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        task_anchor: String,
    },
    /// A subagent started executing its assigned task.
    SubagentStarted {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
    },
    /// A subagent reported a provider-neutral status update.
    SubagentStatusChanged {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        status: crate::SubagentStatus,
    },
    /// A subagent completed and reported compact references to its work.
    SubagentCompleted {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        summary: String,
        output_paths: Vec<String>,
        changed_paths: Vec<String>,
    },
    /// A subagent failed.
    SubagentFailed {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        diagnostic: ErrorInfo,
    },
    /// A subagent was cancelled.
    SubagentCancelled {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        diagnostic: ErrorInfo,
    },
    /// The runtime was cancelled.
    Cancelled { diagnostic: ErrorInfo },
    /// The runtime failed.
    Failed { diagnostic: ErrorInfo },
}
