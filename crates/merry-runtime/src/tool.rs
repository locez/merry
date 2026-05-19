//! Runtime-owned tool execution API and registry.
//!
//! [`ToolExecutor`] is an outcome-only boundary. Executors should run tool
//! infrastructure and return [`ToolExecutionOutcome`]; they should not call
//! runtime mutation APIs as callbacks. [`crate::Runtime::execute_tool_call`]
//! already owns the active runtime step permit while the executor runs, so
//! reentrant mutation attempts are rejected by normal step admission.

use crate::ArtifactContent;
use merry_core::{ErrorInfo, PendingToolCall, ToolCallResultStatus, ToolName, ToolSpec};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Boxed tool executor future used for object-safe async tool boundaries.
pub type ToolExecutorFuture<'a> = Pin<Box<dyn Future<Output = ToolExecutionResult> + Send + 'a>>;

/// Result returned by a runtime-owned tool executor.
///
/// [`ToolExecutionError`] represents executor infrastructure failure or
/// cooperative cancellation. Tool-domain failures should be returned as a
/// failed [`ToolExecutionOutcome`] so runtime can durably resolve the pending
/// tool call.
pub type ToolExecutionResult = Result<ToolExecutionOutcome, ToolExecutionError>;

/// Context passed to a tool executor.
#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    cancellation_token: CancellationToken,
}

impl ToolExecutionContext {
    /// Creates a tool execution context with the provided cancellation token.
    #[must_use]
    pub fn new(cancellation_token: CancellationToken) -> Self {
        Self { cancellation_token }
    }

    /// Returns the cancellation token for this tool execution.
    #[must_use]
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }
}

impl Default for ToolExecutionContext {
    fn default() -> Self {
        Self {
            cancellation_token: CancellationToken::new(),
        }
    }
}

/// Object-safe runtime tool executor boundary.
///
/// The executor returns content and status only. Runtime code records the
/// artifact, emits events, updates the ledger, and resolves the pending call.
pub trait ToolExecutor: Send + Sync {
    /// Executes one pending model-requested tool call.
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a>;
}

/// Domain-level result from a tool execution.
///
/// Runtime code turns this into a stable artifact reference and a
/// `ToolCallResult`; executors only provide the exact text or JSON payload.
/// This type intentionally carries no artifact id, event, or ledger update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionOutcome {
    status: ToolCallResultStatus,
    content: ArtifactContent,
    diagnostic: Option<ErrorInfo>,
}

impl ToolExecutionOutcome {
    /// Creates a successful text result.
    #[must_use]
    pub fn succeeded_text(content: impl Into<String>) -> Self {
        Self::succeeded(ArtifactContent::text(content))
    }

    /// Creates a successful JSON result.
    #[must_use]
    pub fn succeeded_json(content: impl Into<String>) -> Self {
        Self::succeeded(ArtifactContent::json(content))
    }

    /// Creates a failed text result with a small diagnostic.
    #[must_use]
    pub fn failed_text(content: impl Into<String>, diagnostic: ErrorInfo) -> Self {
        Self::failed(ArtifactContent::text(content), diagnostic)
    }

    /// Creates a failed JSON result with a small diagnostic.
    #[must_use]
    pub fn failed_json(content: impl Into<String>, diagnostic: ErrorInfo) -> Self {
        Self::failed(ArtifactContent::json(content), diagnostic)
    }

    /// Returns the tool execution status.
    #[must_use]
    pub fn status(&self) -> ToolCallResultStatus {
        self.status
    }

    /// Borrows the exact execution content.
    #[must_use]
    pub fn content(&self) -> &ArtifactContent {
        &self.content
    }

    /// Borrows the optional failure diagnostic.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&ErrorInfo> {
        self.diagnostic.as_ref()
    }

    pub(crate) fn into_parts(self) -> (ToolCallResultStatus, ArtifactContent, Option<ErrorInfo>) {
        (self.status, self.content, self.diagnostic)
    }

    fn succeeded(content: ArtifactContent) -> Self {
        Self {
            status: ToolCallResultStatus::Succeeded,
            content,
            diagnostic: None,
        }
    }

    fn failed(content: ArtifactContent, diagnostic: ErrorInfo) -> Self {
        Self {
            status: ToolCallResultStatus::Failed,
            content,
            diagnostic: Some(diagnostic),
        }
    }
}

/// Infrastructure-level errors raised by tool executors.
///
/// Use this for cancellation or infrastructure failures only. If the tool ran
/// and produced a domain-level failure, return a failed [`ToolExecutionOutcome`]
/// instead.
#[derive(Debug, Error)]
pub enum ToolExecutionError {
    /// Tool execution was cancelled cooperatively.
    #[error("tool execution cancelled")]
    Cancelled,

    /// Tool execution could not complete because the executor infrastructure failed.
    #[error("tool execution infrastructure error: {message}")]
    Infrastructure {
        /// Actionable executor error detail.
        message: String,
    },
}

impl ToolExecutionError {
    /// Creates an infrastructure error.
    #[must_use]
    pub fn infrastructure(message: impl Into<String>) -> Self {
        Self::Infrastructure {
            message: message.into(),
        }
    }
}

/// Runtime-owned registered tool definition.
///
/// A registered tool binds a provider-visible spec to an executor. It does not
/// start or automate a tool loop. [`crate::Runtime::submit_tool_result`] is the
/// external/manual result path; [`crate::Runtime::execute_tool_call`] is the
/// runtime-registered executor path.
#[derive(Clone)]
pub struct RegisteredTool {
    spec: ToolSpec,
    executor: Arc<dyn ToolExecutor>,
}

impl RegisteredTool {
    /// Creates a registered tool from its provider-visible spec and runtime executor.
    #[must_use]
    pub fn new(spec: ToolSpec, executor: Arc<dyn ToolExecutor>) -> Self {
        Self { spec, executor }
    }

    /// Borrows the provider-visible tool specification.
    #[must_use]
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    pub(crate) fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::clone(&self.executor)
    }
}

impl std::fmt::Debug for RegisteredTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredTool")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolRegistry {
    tools: BTreeMap<ToolName, RegisteredTool>,
}

impl ToolRegistry {
    pub(crate) fn from_registered(tools: Vec<RegisteredTool>) -> Result<Self, DuplicateToolName> {
        let mut registry = BTreeMap::new();

        for tool in tools {
            let name = tool.spec().name().clone();
            if registry.insert(name.clone(), tool).is_some() {
                return Err(DuplicateToolName { name });
            }
        }

        Ok(Self { tools: registry })
    }

    pub(crate) fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|tool| tool.spec().clone())
            .collect()
    }

    pub(crate) fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        self.tools.get(name).map(RegisteredTool::executor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DuplicateToolName {
    pub(crate) name: ToolName,
}
