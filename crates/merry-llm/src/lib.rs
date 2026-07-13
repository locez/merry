//! Provider-facing traits and normalized model events for Merry.

pub mod capability;
pub mod content;
pub mod error;
pub mod event;
pub mod model_catalog;
pub mod provider;
pub mod request;
pub mod response;
pub mod retry;
#[cfg(any(test, feature = "test-utils"))]
pub mod testing;
pub mod tool;
pub mod usage;

pub use capability::ModelCapabilities;
pub use content::{ModelContent, ModelContentPart, ModelImage};
pub use error::{ModelError, ProviderErrorKind};
pub use event::ModelEvent;
pub use model_catalog::{
    ModelCatalog, ModelCatalogEntry, ModelCatalogError, ModelCatalogErrorKind, ModelCatalogFuture,
    ModelCatalogProvider,
};
pub use provider::{ModelEventStream, ModelProvider, ModelProviderFuture, ModelStreamContext};
pub use request::{
    GenerationConfig, ModelInputItem, ModelMessage, ModelMessageRole, ModelName, ModelRequest,
    ModelResponseFormat, ModelStructuredOutputFormat, ParallelToolCalls, ReasoningEffort,
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
