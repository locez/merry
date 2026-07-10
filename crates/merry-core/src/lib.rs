//! Shared Merry protocol types and runtime contracts.

pub mod artifact;
pub mod error;
pub mod event;
pub mod evidence;
pub mod id;
pub mod journal;
pub mod runtime_event;
pub mod schema;
pub mod tool;
pub mod usage;

pub use artifact::{ArtifactKind, ArtifactRef};
pub use error::CoreError;
pub use event::{ErrorInfo, MerryErrorDomain, MerryErrorInfo, MerryRetryability};
pub use evidence::{EvidenceLocator, EvidenceRef};
pub use id::{
    ArtifactId, ProviderName, SessionId, SkillId, SubagentId, SubagentTaskId, ToolCallBatchId,
    ToolCallId, ToolName,
};
pub use journal::{RuntimeJournalEvent, RuntimeJournalPayload};
pub use runtime_event::{
    InteractiveRunState, QueuedInputLane, QueuedInputView, QueuedInputsView, RuntimeEvent,
    RuntimeEventSource, SubagentStatus, ToolOutput,
};
pub use schema::ToolInputSchema;
pub use tool::{
    PendingToolCall, PendingToolCallBatch, ToolCallArguments, ToolCallResult, ToolCallResultStatus,
    ToolSpec,
};
pub use usage::{
    CompactionUsageWindow, ContextWindowSource, ModelUsage, SessionUsage, UsageContextWindow,
};
