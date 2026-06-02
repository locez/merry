//! Shared Merry protocol types and runtime contracts.

pub mod artifact;
pub mod error;
pub mod event;
pub mod evidence;
pub mod id;
pub mod schema;
pub mod tool;

pub use artifact::{ArtifactKind, ArtifactRef};
pub use error::CoreError;
pub use event::{
    ErrorInfo, MerryErrorDomain, MerryErrorInfo, MerryRetryability, RuntimeEvent, RuntimeEventKind,
};
pub use evidence::{EvidenceLocator, EvidenceRef};
pub use id::{
    ArtifactId, ProviderName, SessionId, SkillId, SubagentId, SubagentTaskId, ToolCallId, ToolName,
};
pub use schema::ToolInputSchema;
pub use tool::{
    PendingToolCall, ToolCallArguments, ToolCallResult, ToolCallResultStatus, ToolSpec,
};
