use crate::{UserImageInput, tool::proposal::ActionExecutionEvidence};
use merry_core::{ErrorInfo, ToolCallResultStatus, ToolOutput};

/// Domain-level result from a tool execution.
///
/// Executors supply full output and may omit its presentation-only envelope
/// from a separate model body. Runtime owns persistence, events, and ledger
/// updates; an absent model body preserves the full result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionOutcome {
    status: ToolCallResultStatus,
    content: ToolResultContent,
    diagnostic: Option<ErrorInfo>,
    execution_evidence: Option<ActionExecutionEvidence>,
}

/// Full output, optional model body, and optional normalized PNG images, kept together through admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolResultContent {
    full: ToolOutput,
    model: Option<Box<ToolOutput>>,
    images: Vec<UserImageInput>,
}

impl From<ToolOutput> for ToolResultContent {
    fn from(full: ToolOutput) -> Self {
        Self {
            full,
            model: None,
            images: Vec::new(),
        }
    }
}

impl ToolResultContent {
    pub(crate) fn full(&self) -> &ToolOutput {
        &self.full
    }

    pub(crate) fn model(&self) -> Option<&ToolOutput> {
        self.model.as_deref()
    }

    pub(crate) fn images(&self) -> &[UserImageInput] {
        &self.images
    }

    pub(crate) fn with_model(mut self, model: ToolOutput) -> Self {
        self.model = Some(Box::new(model));
        self
    }

    pub(crate) fn with_image(mut self, image: UserImageInput) -> Self {
        self.images.push(image);
        self
    }

    pub(crate) fn with_images<I>(mut self, images: I) -> Self
    where
        I: IntoIterator<Item = UserImageInput>,
    {
        self.images.extend(images);
        self
    }

    pub(crate) fn into_parts(self) -> (ToolOutput, Option<ToolOutput>, Vec<UserImageInput>) {
        (self.full, self.model.map(|content| *content), self.images)
    }
}

impl ToolExecutionOutcome {
    /// Creates a successful text result.
    #[must_use]
    pub fn succeeded_text(content: impl Into<String>) -> Self {
        Self::succeeded(ToolOutput::text(content))
    }

    /// Creates a successful JSON result.
    #[must_use]
    pub fn succeeded_json(content: impl Into<String>) -> Self {
        Self::succeeded(ToolOutput::json(content))
    }

    /// Creates a failed text result with a small diagnostic.
    ///
    /// Use this when the tool ran and produced a domain-level failure that
    /// should resolve the pending call durably.
    #[must_use]
    pub fn failed_text(content: impl Into<String>, diagnostic: ErrorInfo) -> Self {
        Self::failed(ToolOutput::text(content), diagnostic)
    }

    /// Creates a failed JSON result with a small diagnostic.
    ///
    /// Use this when the tool ran and produced a domain-level failure that
    /// should resolve the pending call durably.
    #[must_use]
    pub fn failed_json(content: impl Into<String>, diagnostic: ErrorInfo) -> Self {
        Self::failed(ToolOutput::json(content), diagnostic)
    }

    /// Returns the tool execution status.
    #[must_use]
    pub fn status(&self) -> ToolCallResultStatus {
        self.status
    }

    /// Borrows the exact full execution output.
    ///
    /// Runtime records this output before emitting the resolution event.
    #[must_use]
    pub fn content(&self) -> &ToolOutput {
        self.content.full()
    }

    /// Borrows the optional model-facing body instead of the full output.
    ///
    /// Runtime persists this body for request replay and compaction. Full tool
    /// output reads and presentation retain [`Self::content`].
    #[must_use]
    pub fn model_content(&self) -> Option<&ToolOutput> {
        self.content.model()
    }

    /// Supplies model-facing text without changing the full result output.
    ///
    /// Keep execution status, useful output, and completeness warnings in this
    /// body. Runtime rejects blank bodies before resolving the tool call.
    #[must_use]
    pub fn with_model_text(mut self, content: impl Into<String>) -> Self {
        self.content = self.content.with_model(ToolOutput::text(content));
        self
    }

    /// Supplies model-facing JSON without changing the full result output.
    ///
    /// The caller owns the JSON schema and must preserve actionable results.
    /// Runtime rejects blank bodies before resolving the tool call.
    #[must_use]
    pub fn with_model_json(mut self, content: impl Into<String>) -> Self {
        self.content = self.content.with_model(ToolOutput::json(content));
        self
    }

    /// Attaches one normalized PNG image observed alongside this tool outcome.
    #[must_use]
    pub fn with_image(mut self, image: UserImageInput) -> Self {
        self.content = self.content.with_image(image);
        self
    }

    /// Attaches normalized PNG images observed alongside this tool outcome.
    #[must_use]
    pub fn with_images<I>(mut self, images: I) -> Self
    where
        I: IntoIterator<Item = UserImageInput>,
    {
        self.content = self.content.with_images(images);
        self
    }

    /// Borrows normalized PNG images attached to this tool outcome.
    #[must_use]
    pub fn images(&self) -> &[UserImageInput] {
        self.content.images()
    }

    /// Borrows the optional failure diagnostic.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&ErrorInfo> {
        self.diagnostic.as_ref()
    }

    /// Borrows provider-invisible evidence produced by the actual execution.
    ///
    /// Runtime validates this internally against the approved proposal. It must
    /// not be rendered into tool results, provider continuations, or provider
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

    pub(super) fn succeeded(content: ToolOutput) -> Self {
        Self {
            status: ToolCallResultStatus::Succeeded,
            content: content.into(),
            diagnostic: None,
            execution_evidence: None,
        }
    }

    pub(super) fn failed(content: ToolOutput, diagnostic: ErrorInfo) -> Self {
        Self {
            status: ToolCallResultStatus::Failed,
            content: content.into(),
            diagnostic: Some(diagnostic),
            execution_evidence: None,
        }
    }
}
