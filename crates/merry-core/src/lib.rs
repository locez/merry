//! Shared Merry protocol types and runtime contracts.

pub mod artifact;
pub mod error;
pub mod event;
pub mod evidence;
pub mod id;
pub mod journal;
pub mod plan;
pub mod runtime_event;
pub mod schema;
pub mod subagent_activity;
pub mod tool;
pub mod usage;

pub use artifact::{ArtifactKind, ArtifactRef};
pub use error::CoreError;
pub use event::{ErrorInfo, MerryErrorDomain, MerryErrorInfo, MerryRetryability};
pub use evidence::{EvidenceLocator, EvidenceRef};
pub use id::{
    ArtifactId, PlanApprovalRequirementId, PlanAttemptId, PlanBindingId, PlanDirectiveId, PlanId,
    PlanLeaseId, PlanNodeId, ProviderName, SessionId, SkillId, SubagentId, SubagentTaskId,
    ToolCallBatchId, ToolCallId, ToolName,
};
pub use journal::{RuntimeJournalEvent, RuntimeJournalPayload};
pub use plan::{
    CoordinatorDirectiveSnapshot, PlanActivationSource, PlanApprovalRequirementKind,
    PlanApprovalRequirementSnapshot, PlanApprovalRequirementStatus, PlanAttemptOutcome,
    PlanAttemptProgressSnapshot, PlanAttemptSnapshot, PlanCapabilityEnvelopeSnapshot,
    PlanDirectiveConstraints, PlanDirectiveKind, PlanDirectiveStatus, PlanEffectiveNodeStatus,
    PlanExecutionSummary, PlanExecutorPolicy, PlanHarnessSnapshot, PlanLeaseSnapshot,
    PlanLeaseStatus, PlanLinkSnapshot, PlanLinkStatus, PlanNodeResult, PlanNodeSnapshot,
    PlanNodeStatus, PlanPhase, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot,
    PlanRevisionSummary, PlanSchedulerStatus, PlanSnapshot,
};
pub use runtime_event::{
    InteractiveRunState, QueuedInputLane, QueuedInputView, QueuedInputsView, RuntimeEvent,
    RuntimeEventSource, SubagentStatus, ToolOutput,
};
pub use schema::ToolInputSchema;
pub use subagent_activity::{SubagentActivityPhase, SubagentActivitySnapshot};
pub use tool::{
    PendingToolCall, PendingToolCallBatch, TOOL_CANCELLED_BY_USER_CODE, ToolCallArguments,
    ToolCallResult, ToolCallResultStatus, ToolSpec,
};
pub use usage::{
    CompactionUsageWindow, ContextWindowSource, ModelUsage, SessionUsage, UsageContextWindow,
};
