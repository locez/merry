//! Provider-facing traits and normalized model events for Merry.

pub mod capability;
pub mod error;
pub mod event;
pub mod provider;
pub mod request;
pub mod response;
pub mod retry;
#[cfg(any(test, feature = "test-utils"))]
pub mod testing;
pub mod tool;
pub mod usage;

pub use capability::ModelCapabilities;
pub use error::{ModelError, ProviderErrorKind};
pub use event::ModelEvent;
pub use provider::{ModelEventStream, ModelProvider, ModelProviderFuture, ModelStreamContext};
pub use request::{
    GenerationConfig, ModelContent, ModelInputItem, ModelMessage, ModelMessageRole, ModelName,
    ModelRequest, ModelResponseFormat, ModelStructuredOutputFormat, ReasoningEffort,
    RequestContentHash, ToolProfileHash,
};
pub use response::{FinishReason, ModelOutput, ModelResponse};
pub use retry::{
    ModelRetryEvent, ModelRetryEventStream, ModelRetryPolicy, ModelRetryPolicyError,
    RetryModelStreamContext, RetryingModelProvider,
};
pub use tool::{
    ModelToolBatchContinuation, ModelToolCall, ModelToolCallBatch, ModelToolCallId,
    ModelToolContinuation, ModelToolResult, ModelToolResultContent, ToolArguments,
};
pub use usage::Usage;
