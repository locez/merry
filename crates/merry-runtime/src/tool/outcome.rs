use crate::{ArtifactContent, tool::proposal::ActionExecutionEvidence};
use merry_core::{ErrorInfo, ToolCallResultStatus};

/// Domain-level result from a tool execution.
///
/// Executors supply full evidence and may omit its presentation-only envelope
/// from a separate model body. Runtime owns artifact ids, persistence, events,
/// and ledger updates; an absent model body preserves the full result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionOutcome {
    status: ToolCallResultStatus,
    content: ToolResultContent,
    diagnostic: Option<ErrorInfo>,
    execution_evidence: Option<ActionExecutionEvidence>,
}

/// Full evidence and an optional model body, kept together through admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolResultContent {
    artifact: ArtifactContent,
    model: Option<Box<ArtifactContent>>,
}

impl From<ArtifactContent> for ToolResultContent {
    fn from(artifact: ArtifactContent) -> Self {
        Self {
            artifact,
            model: None,
        }
    }
}

impl ToolResultContent {
    pub(crate) fn artifact(&self) -> &ArtifactContent {
        &self.artifact
    }

    pub(crate) fn model(&self) -> Option<&ArtifactContent> {
        self.model.as_deref()
    }

    pub(crate) fn with_model(mut self, model: ArtifactContent) -> Self {
        self.model = Some(Box::new(model));
        self
    }

    pub(crate) fn into_parts(self) -> (ArtifactContent, Option<ArtifactContent>) {
        (self.artifact, self.model.map(|content| *content))
    }
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
    ///
    /// Use this when the tool ran and produced a domain-level failure that
    /// should resolve the pending call durably.
    #[must_use]
    pub fn failed_text(content: impl Into<String>, diagnostic: ErrorInfo) -> Self {
        Self::failed(ArtifactContent::text(content), diagnostic)
    }

    /// Creates a failed JSON result with a small diagnostic.
    ///
    /// Use this when the tool ran and produced a domain-level failure that
    /// should resolve the pending call durably.
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
    ///
    /// Runtime records this content before emitting the resolution event.
    #[must_use]
    pub fn content(&self) -> &ArtifactContent {
        self.content.artifact()
    }

    /// Borrows the optional model-facing body instead of the full artifact.
    ///
    /// Runtime persists this body for request replay and compaction. Artifact
    /// reads, evidence references, and presentation retain [`Self::content`].
    #[must_use]
    pub fn model_content(&self) -> Option<&ArtifactContent> {
        self.content.model()
    }

    /// Supplies model-facing text without changing the full result artifact.
    ///
    /// Keep execution status, useful output, and completeness warnings in this
    /// body. Runtime rejects blank bodies before resolving the tool call.
    #[must_use]
    pub fn with_model_text(mut self, content: impl Into<String>) -> Self {
        self.content = self.content.with_model(ArtifactContent::text(content));
        self
    }

    /// Supplies model-facing JSON without changing the full result artifact.
    ///
    /// The caller owns the JSON schema and must preserve actionable results.
    /// Runtime rejects blank bodies before resolving the tool call.
    #[must_use]
    pub fn with_model_json(mut self, content: impl Into<String>) -> Self {
        self.content = self.content.with_model(ArtifactContent::json(content));
        self
    }

    /// Borrows the optional failure diagnostic.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&ErrorInfo> {
        self.diagnostic.as_ref()
    }

    /// Borrows provider-invisible evidence produced by the actual execution.
    ///
    /// Runtime records this only in internal action audit state. It must not be
    /// rendered into tool result artifacts, provider continuations, or provider
    /// request payloads.
    #[must_use]
    pub fn execution_evidence(&self) -> Option<&ActionExecutionEvidence> {
        self.execution_evidence.as_ref()
    }

    /// Attaches provider-invisible evidence from the actual execution.
    #[must_use]
    pub fn with_execution_evidence(mut self, evidence: ActionExecutionEvidence) -> Self {
        self.execution_evidence = Some(evidence);
        self
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ToolCallResultStatus,
        ToolResultContent,
        Option<ErrorInfo>,
        Option<ActionExecutionEvidence>,
    ) {
        (
            self.status,
            self.content,
            self.diagnostic,
            self.execution_evidence,
        )
    }

    pub(super) fn succeeded(content: ArtifactContent) -> Self {
        Self {
            status: ToolCallResultStatus::Succeeded,
            content: content.into(),
            diagnostic: None,
            execution_evidence: None,
        }
    }

    pub(super) fn failed(content: ArtifactContent, diagnostic: ErrorInfo) -> Self {
        Self {
            status: ToolCallResultStatus::Failed,
            content: content.into(),
            diagnostic: Some(diagnostic),
            execution_evidence: None,
        }
    }
}
