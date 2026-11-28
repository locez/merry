//! Shared Merry protocol types and runtime contracts.

pub mod error;
pub mod event;
pub mod host_paths;
pub mod id;
pub mod journal;
pub mod output_rate;
pub mod plan;
pub mod runtime_event;
pub mod schema;
pub mod subagent_activity;
pub mod todo;
pub mod tool;
pub mod tool_catalog;
pub mod trajectory;
pub mod usage;

pub use error::CoreError;
pub use event::{ErrorInfo, MerryErrorDomain, MerryErrorInfo, MerryRetryability};
pub use host_paths::ProtectedHostPaths;
pub use id::{
    PlanId, ProviderName, SessionId, SkillId, SubagentId, SubagentTaskId, ToolAdapterId,
    ToolBindingName, ToolCallBatchId, ToolCallId, ToolName, ToolSourceFingerprint, ToolSourceId,
    TrajectoryRecordId,
};
pub use journal::{RuntimeJournalEvent, RuntimeJournalPayload};
pub use output_rate::{ModelOutputRate, OutputTimingQuality, OutputTokenSource};
pub use plan::{
    PlanDocument, PlanDocumentStatus, PlanDocumentSummary, PlanModeState, PlanReviewDecision,
    PlanSessionSnapshot,
};
pub use runtime_event::{
    ImageSummary, InteractiveRunState, QueuedInputView, QueuedInputsView, RuntimeEvent,
    RuntimeEventSource, SubagentStatus, ToolOutput, ToolOutputKind, ToolOutputPreview,
    ToolOutputStream,
};
pub use schema::ToolInputSchema;
pub use subagent_activity::{SubagentActivityPhase, SubagentActivitySnapshot};
pub use todo::{MAX_TODO_CONTENT_BYTES, MAX_TODO_ID_BYTES, MAX_TODO_ITEMS, TodoItem, TodoStatus};
pub use tool::{
    PendingToolCall, PendingToolCallBatch, TOOL_CANCELLED_BY_USER_CODE, ToolCallArguments,
    ToolCallResult, ToolCallResultStatus, ToolSpec,
};
pub use tool_catalog::{ExternalToolBinding, SessionToolCatalog, SessionToolCatalogEntry};
pub use trajectory::{
    TrajectoryEvent, TrajectoryLane, TrajectoryPayload, TrajectoryPayloadKind,
    TrajectoryPromptBlock, TrajectoryPromptSnapshot, TrajectoryRecord, TrajectoryRecordDetails,
    TrajectoryRecordKind, TrajectoryRecordStatus, TrajectorySnapshot, TrajectoryToolDetails,
    TrajectoryTurnId,
};
pub use usage::{
    CompactionUsageWindow, ContextWindowSource, ModelUsage, SessionUsage, UsageContextWindow,
};
