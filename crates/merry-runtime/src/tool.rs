//! Runtime-owned tool execution API and registry.
//!
//! [`ToolExecutor`] is an outcome-only boundary. Executors should run tool
//! infrastructure and return [`ToolExecutionOutcome`]; they should not call
//! runtime mutation APIs as callbacks. [`crate::Runtime::execute_tool_call`]
//! already owns the active runtime step permit while the executor runs, so
//! reentrant mutation attempts are rejected by normal step admission.
//!
//! Tool calls and results are provider-neutral Merry values. Provider adapters
//! render tool specs and continuations into provider wire formats outside this
//! crate.

use crate::{
    ArtifactContent, ProcessActionError, ProcessActionIntent, ProcessExecutionEvidence,
    tool_input_validation::{CompiledToolInputValidator, ToolInputValidationError},
};
use merry_core::{
    ErrorInfo, PendingToolCall, ToolCallId, ToolCallResultStatus, ToolName, ToolSpec,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Component, Path},
    pin::Pin,
    sync::Arc,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Boxed tool executor future used for object-safe async tool boundaries.
///
/// Public tool boundaries use an explicit boxed future so registered executors
/// can be stored behind [`ToolExecutor`].
pub type ToolExecutorFuture<'a> = Pin<Box<dyn Future<Output = ToolExecutionResult> + Send + 'a>>;

/// Boxed action proposal future used for object-safe async tool boundaries.
///
/// The proposal hook is runtime-owned and provider-neutral. It is intentionally
/// not rendered into provider tool specs or runtime events.
pub type ToolActionProposalFuture<'a> =
    Pin<Box<dyn Future<Output = ToolActionProposalResult> + Send + 'a>>;

/// Result returned by a runtime-owned tool executor.
///
/// [`ToolExecutionError`] represents executor infrastructure failure or
/// cooperative cancellation. Tool-domain failures should be returned as a
/// failed [`ToolExecutionOutcome`] so runtime can durably resolve the pending
/// tool call.
pub type ToolExecutionResult = Result<ToolExecutionOutcome, ToolExecutionError>;

/// Optional proposal returned before a write-classified tool can be resolved by policy.
///
/// `NoProposal` means the executor cannot provide deterministic proposal
/// evidence for this call and policy should continue with its normal decision
/// path.
pub type ToolActionProposalResult = Result<ToolActionPreflight, ToolExecutionError>;

/// Where a registered tool is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRunner {
    /// Merry/Rust runtime owns execution through a [`ToolExecutor`].
    Runtime,
    /// An external SDK runner executes after Merry emits a bridge event.
    Bridge,
}

/// Runtime execution policy for calls that appear in the same model batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolConcurrency {
    /// Calls may execute concurrently with adjacent parallel-safe calls.
    ParallelSafe,
    /// The call executes alone and acts as a barrier between concurrent waves.
    Exclusive,
}

/// Context passed to a tool executor.
///
/// The context is intentionally small for the MVP: cancellation is cooperative
/// and runtime state mutation stays owned by [`crate::Runtime::execute_tool_call`].
#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    cancellation_token: CancellationToken,
    approved_workspace_patch: Option<WorkspacePatchProposal>,
}

impl ToolExecutionContext {
    /// Creates a tool execution context with the provided cancellation token.
    #[must_use]
    pub fn new(cancellation_token: CancellationToken) -> Self {
        Self {
            cancellation_token,
            approved_workspace_patch: None,
        }
    }

    /// Returns the cancellation token for this tool execution.
    ///
    /// Executors should check this token at cancellation points and return
    /// [`ToolExecutionError::Cancelled`] when no durable result was produced.
    #[must_use]
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    /// Borrows the approved workspace patch proposal for this execution.
    ///
    /// This is executor-internal runtime state. It is never rendered into
    /// provider-visible tool specs, tool result artifacts, or continuations.
    #[must_use]
    pub fn approved_workspace_patch(&self) -> Option<&WorkspacePatchProposal> {
        self.approved_workspace_patch.as_ref()
    }

    pub(crate) fn with_approved_workspace_patch(mut self, patch: WorkspacePatchProposal) -> Self {
        self.approved_workspace_patch = Some(patch);
        self
    }
}

impl Default for ToolExecutionContext {
    fn default() -> Self {
        Self {
            cancellation_token: CancellationToken::new(),
            approved_workspace_patch: None,
        }
    }
}

/// Object-safe runtime tool executor boundary.
///
/// The executor returns content and status only. Runtime code records the
/// artifact, emits events, updates the ledger, and resolves the pending call.
///
/// Implementations should not call runtime mutation APIs from inside
/// [`ToolExecutor::execute`]. The runtime already owns the active-step permit
/// while this method runs.
pub trait ToolExecutor: Send + Sync {
    /// Builds read-only deterministic evidence for a proposed action.
    ///
    /// This hook must not mutate workspace, runtime, network, or process state.
    /// It exists so runtime can record a provider-neutral proposal before a
    /// mutating action is denied, approved, or otherwise reviewed. Existing
    /// tools can omit this method; the default returns no proposal.
    fn propose<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolActionProposalFuture<'a> {
        Box::pin(async { Ok(ToolActionPreflight::NoProposal) })
    }

    /// Executes one pending model-requested tool call.
    ///
    /// The pending call uses Merry-owned ids, tool names, and arguments rather
    /// than provider response structs.
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
    execution_evidence: Option<ActionExecutionEvidence>,
}

/// Result of a tool action preflight/proposal hook.
///
/// Mutating tools use this hook before runtime policy decides whether execution
/// is allowed. Most tools return a proposal or no proposal. Tools may return a
/// durable failed outcome for provider-supplied argument errors discovered
/// during preflight, so the model receives actionable feedback instead of an
/// infrastructure failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolActionPreflight {
    /// The tool did not provide deterministic proposal evidence.
    NoProposal,
    /// The tool provided proposal evidence for policy review.
    Proposal(ActionProposal),
    /// The tool preflight produced a durable tool outcome.
    Outcome(ToolExecutionOutcome),
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
    /// Runtime code records this content into a generated artifact before
    /// emitting the resolution event.
    #[must_use]
    pub fn content(&self) -> &ArtifactContent {
        &self.content
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
        ArtifactContent,
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

    fn succeeded(content: ArtifactContent) -> Self {
        Self {
            status: ToolCallResultStatus::Succeeded,
            content,
            diagnostic: None,
            execution_evidence: None,
        }
    }

    fn failed(content: ArtifactContent, diagnostic: ErrorInfo) -> Self {
        Self {
            status: ToolCallResultStatus::Failed,
            content,
            diagnostic: Some(diagnostic),
            execution_evidence: None,
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
    ///
    /// Infrastructure errors leave the pending tool call unresolved.
    #[must_use]
    pub fn infrastructure(message: impl Into<String>) -> Self {
        Self::Infrastructure {
            message: message.into(),
        }
    }
}

/// Runtime-owned action category for registered tools.
///
/// This metadata is intentionally not part of provider-visible [`ToolSpec`].
/// Runtime policy uses it before invoking an executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolActionKind {
    /// Reads runtime or workspace state without changing it.
    ReadOnly,
    /// Mutates runtime-owned control state without direct external side effects.
    RuntimeControl,
    /// Writes files or other state in the configured workspace.
    WorkspaceWrite,
    /// Executes a local command or process.
    CommandExec,
    /// Uses network access.
    Network,
    /// Executes a user-configured external tool that is trusted by configuration.
    TrustedExternal,
}

impl ToolActionKind {
    /// Returns whether this action category can cause external side effects
    /// that require the mutating action commit lifecycle.
    #[must_use]
    pub fn is_mutating(self) -> bool {
        matches!(
            self,
            Self::WorkspaceWrite | Self::CommandExec | Self::Network
        )
    }
}

/// Runtime-owned, provider-neutral proposal for a mutating registered action.
///
/// This type is public only so tool crates can supply deterministic proposal
/// evidence to `merry-runtime`. It is unstable implementation-facing API: it is
/// not part of `merry_core::RuntimeJournalEvent`, is not provider wire format, and must
/// not be rendered into provider-visible tool specs or continuations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionProposal {
    tool_call_id: ToolCallId,
    tool_name: ToolName,
    action_kind: ToolActionKind,
    label: String,
    subject: String,
    summary: String,
    evidence: ActionProposalEvidence,
}

impl ActionProposal {
    /// Creates a validated mutating action proposal for a pending tool call.
    pub fn new(
        call: &PendingToolCall,
        action_kind: ToolActionKind,
        label: impl Into<String>,
        subject: impl Into<String>,
        summary: impl Into<String>,
        evidence: ActionProposalEvidence,
    ) -> Result<Self, ActionProposalError> {
        if action_kind == ToolActionKind::ReadOnly {
            return Err(ActionProposalError::ReadOnlyAction);
        }
        validate_proposal_evidence_matches_action_kind(action_kind, &evidence)?;

        Ok(Self {
            tool_call_id: call.id().clone(),
            tool_name: call.name().clone(),
            action_kind,
            label: validate_compact_proposal_text("label", label.into())?,
            subject: validate_compact_proposal_text("subject", subject.into())?,
            summary: validate_compact_proposal_text("summary", summary.into())?,
            evidence,
        })
    }

    /// Returns the proposed tool call id.
    #[must_use]
    pub fn tool_call_id(&self) -> &ToolCallId {
        &self.tool_call_id
    }

    /// Returns the proposed tool name.
    #[must_use]
    pub fn tool_name(&self) -> &ToolName {
        &self.tool_name
    }

    /// Returns the proposed runtime action kind.
    #[must_use]
    pub fn action_kind(&self) -> ToolActionKind {
        self.action_kind
    }

    /// Returns the compact proposal label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the compact proposal subject.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Returns the compact proposal summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Returns provider-neutral deterministic proposal evidence.
    #[must_use]
    pub fn evidence(&self) -> &ActionProposalEvidence {
        &self.evidence
    }

    /// Returns a copy suitable for internal action audit storage.
    ///
    /// Proposal audits keep deterministic identity, but process proposals must
    /// not retain inline stdin payloads.
    #[must_use]
    pub(crate) fn audit_sanitized(&self) -> Self {
        let evidence = match &self.evidence {
            ActionProposalEvidence::WorkspacePatch(patch) => {
                ActionProposalEvidence::WorkspacePatch(patch.clone())
            }
            ActionProposalEvidence::ProcessAction(intent) => {
                ActionProposalEvidence::ProcessAction(intent.without_stdin_text())
            }
        };

        Self {
            tool_call_id: self.tool_call_id.clone(),
            tool_name: self.tool_name.clone(),
            action_kind: self.action_kind,
            label: self.label.clone(),
            subject: self.subject.clone(),
            summary: self.summary.clone(),
            evidence,
        }
    }

    pub(crate) fn validate_for_call(
        &self,
        call: &PendingToolCall,
        action_kind: ToolActionKind,
    ) -> Result<(), &'static str> {
        if self.tool_call_id != *call.id() {
            return Err("proposal tool call id does not match pending call");
        }
        if self.tool_name != *call.name() {
            return Err("proposal tool name does not match pending call");
        }
        if self.action_kind != action_kind {
            return Err("proposal action kind does not match registered tool");
        }
        if !self.evidence.matches_action_kind(action_kind) {
            return Err("proposal evidence does not match registered tool action kind");
        }
        Ok(())
    }
}

/// Provider-neutral deterministic evidence attached to an action proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionProposalEvidence {
    /// A constrained workspace patch proposal.
    WorkspacePatch(WorkspacePatchProposal),
    /// A typed local process action intent.
    ProcessAction(ProcessActionIntent),
}

impl ActionProposalEvidence {
    fn matches_action_kind(&self, action_kind: ToolActionKind) -> bool {
        matches!(
            (action_kind, self),
            (ToolActionKind::WorkspaceWrite, Self::WorkspacePatch(_))
                | (ToolActionKind::CommandExec, Self::ProcessAction(_))
        )
    }
}

/// Provider-neutral internal evidence attached after a mutating action executes.
///
/// This evidence is runtime-owned audit state. It intentionally carries no
/// provider wire data and must not be exposed through artifacts or tool result
/// continuations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionExecutionEvidence {
    /// A constrained workspace patch that was actually applied.
    WorkspacePatch(WorkspacePatchExecutionEvidence),
    /// Evidence from a local process action execution.
    ProcessAction(ProcessExecutionEvidence),
}

impl ActionExecutionEvidence {
    pub(crate) fn matches_action_kind(&self, action_kind: ToolActionKind) -> bool {
        matches!(
            (action_kind, self),
            (ToolActionKind::WorkspaceWrite, Self::WorkspacePatch(_))
                | (ToolActionKind::CommandExec, Self::ProcessAction(_))
        )
    }
}

/// Per-file metadata for a constrained workspace patch change.
///
/// This stores only relative workspace identity, byte counts, and stable
/// non-cryptographic content fingerprints. It does not store old or new text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePatchChangeEvidence {
    relative_path: String,
    preimage_bytes: usize,
    replacement_bytes: usize,
    file_bytes_before: usize,
    file_bytes_after: usize,
    file_fingerprint_before: String,
    file_fingerprint_after: String,
}

impl WorkspacePatchChangeEvidence {
    /// Creates validated metadata for one file change in a workspace patch.
    pub fn new(
        relative_path: impl Into<String>,
        preimage_bytes: usize,
        replacement_bytes: usize,
        file_bytes_before: usize,
        file_bytes_after: usize,
        file_fingerprint_before: impl Into<String>,
        file_fingerprint_after: impl Into<String>,
    ) -> Result<Self, ActionProposalError> {
        let relative_path = validate_workspace_patch_relative_path(relative_path.into())?;
        validate_workspace_patch_counts(
            preimage_bytes,
            replacement_bytes,
            file_bytes_before,
            file_bytes_after,
        )?;
        let file_fingerprint_before = validate_workspace_patch_fingerprint(
            "file_fingerprint_before",
            file_fingerprint_before.into(),
        )?;
        let file_fingerprint_after = validate_workspace_patch_fingerprint(
            "file_fingerprint_after",
            file_fingerprint_after.into(),
        )?;

        Ok(Self {
            relative_path,
            preimage_bytes,
            replacement_bytes,
            file_bytes_before,
            file_bytes_after,
            file_fingerprint_before,
            file_fingerprint_after,
        })
    }

    /// Returns the workspace-relative path using `/` separators.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Returns the byte length of the matched preimage.
    #[must_use]
    pub fn preimage_bytes(&self) -> usize {
        self.preimage_bytes
    }

    /// Returns the byte length of the replacement text.
    #[must_use]
    pub fn replacement_bytes(&self) -> usize {
        self.replacement_bytes
    }

    /// Returns the file size immediately before replacement.
    #[must_use]
    pub fn file_bytes_before(&self) -> usize {
        self.file_bytes_before
    }

    /// Returns the file size observed after replacement was written and read back.
    #[must_use]
    pub fn file_bytes_after(&self) -> usize {
        self.file_bytes_after
    }

    /// Returns the stable non-cryptographic fingerprint before replacement.
    #[must_use]
    pub fn file_fingerprint_before(&self) -> &str {
        &self.file_fingerprint_before
    }

    /// Returns the stable non-cryptographic fingerprint after replacement.
    #[must_use]
    pub fn file_fingerprint_after(&self) -> &str {
        &self.file_fingerprint_after
    }
}

/// Execute-time metadata for a constrained workspace patch.
///
/// This stores one or more file changes. It intentionally does not store old or
/// new text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePatchExecutionEvidence {
    changes: Vec<WorkspacePatchChangeEvidence>,
}

impl WorkspacePatchExecutionEvidence {
    /// Creates validated execute-time metadata for a single workspace patch.
    pub fn new(
        relative_path: impl Into<String>,
        preimage_bytes: usize,
        replacement_bytes: usize,
        file_bytes_before: usize,
        file_bytes_after: usize,
        file_fingerprint_before: impl Into<String>,
        file_fingerprint_after: impl Into<String>,
    ) -> Result<Self, ActionProposalError> {
        Self::from_changes(vec![WorkspacePatchChangeEvidence::new(
            relative_path,
            preimage_bytes,
            replacement_bytes,
            file_bytes_before,
            file_bytes_after,
            file_fingerprint_before,
            file_fingerprint_after,
        )?])
    }

    /// Creates execute-time metadata for a multi-file workspace patch.
    pub fn from_changes(
        changes: Vec<WorkspacePatchChangeEvidence>,
    ) -> Result<Self, ActionProposalError> {
        validate_workspace_patch_changes(&changes)?;
        Ok(Self { changes })
    }

    /// Returns all file changes included in this patch.
    #[must_use]
    pub fn changes(&self) -> &[WorkspacePatchChangeEvidence] {
        &self.changes
    }

    fn first_change(&self) -> &WorkspacePatchChangeEvidence {
        self.changes
            .first()
            .expect("workspace patch execution evidence always has at least one change")
    }

    /// Returns the first workspace-relative path using `/` separators.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        self.first_change().relative_path()
    }

    /// Returns the byte length of the first matched preimage.
    #[must_use]
    pub fn preimage_bytes(&self) -> usize {
        self.first_change().preimage_bytes()
    }

    /// Returns the byte length of the first replacement text.
    #[must_use]
    pub fn replacement_bytes(&self) -> usize {
        self.first_change().replacement_bytes()
    }

    /// Returns the first file size immediately before replacement.
    #[must_use]
    pub fn file_bytes_before(&self) -> usize {
        self.first_change().file_bytes_before()
    }

    /// Returns the first file size observed after replacement was written and read back.
    #[must_use]
    pub fn file_bytes_after(&self) -> usize {
        self.first_change().file_bytes_after()
    }

    /// Returns the first stable non-cryptographic fingerprint before replacement.
    #[must_use]
    pub fn file_fingerprint_before(&self) -> &str {
        self.first_change().file_fingerprint_before()
    }

    /// Returns the first stable non-cryptographic fingerprint after replacement.
    #[must_use]
    pub fn file_fingerprint_after(&self) -> &str {
        self.first_change().file_fingerprint_after()
    }
}

/// Deterministic metadata for a constrained workspace patch proposal.
///
/// This stores one or more file changes needed for a future edit decision. It
/// does not store old text, new text, host absolute paths, or provider wire data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePatchProposal {
    changes: Vec<WorkspacePatchChangeEvidence>,
}

impl WorkspacePatchProposal {
    /// Creates validated metadata for a workspace patch file change. New-file
    /// changes use an empty preimage and a zero-byte file state before writing.
    pub fn new(
        relative_path: impl Into<String>,
        preimage_bytes: usize,
        replacement_bytes: usize,
        file_bytes_before: usize,
        file_bytes_after: usize,
        file_fingerprint_before: impl Into<String>,
        file_fingerprint_after: impl Into<String>,
    ) -> Result<Self, ActionProposalError> {
        Self::from_changes(vec![WorkspacePatchChangeEvidence::new(
            relative_path,
            preimage_bytes,
            replacement_bytes,
            file_bytes_before,
            file_bytes_after,
            file_fingerprint_before,
            file_fingerprint_after,
        )?])
    }

    /// Creates proposal metadata for a multi-file workspace patch.
    pub fn from_changes(
        changes: Vec<WorkspacePatchChangeEvidence>,
    ) -> Result<Self, ActionProposalError> {
        validate_workspace_patch_changes(&changes)?;
        Ok(Self { changes })
    }

    /// Returns all file changes included in this patch.
    #[must_use]
    pub fn changes(&self) -> &[WorkspacePatchChangeEvidence] {
        &self.changes
    }

    fn first_change(&self) -> &WorkspacePatchChangeEvidence {
        self.changes
            .first()
            .expect("workspace patch proposal always has at least one change")
    }

    /// Returns the first workspace-relative path using `/` separators.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        self.first_change().relative_path()
    }

    /// Returns the byte length of the first matched preimage.
    #[must_use]
    pub fn preimage_bytes(&self) -> usize {
        self.first_change().preimage_bytes()
    }

    /// Returns the byte length of the first replacement text.
    #[must_use]
    pub fn replacement_bytes(&self) -> usize {
        self.first_change().replacement_bytes()
    }

    /// Returns the first file size before replacement.
    #[must_use]
    pub fn file_bytes_before(&self) -> usize {
        self.first_change().file_bytes_before()
    }

    /// Returns the first projected file size after replacement.
    #[must_use]
    pub fn file_bytes_after(&self) -> usize {
        self.first_change().file_bytes_after()
    }

    /// Returns the first stable non-cryptographic fingerprint before replacement.
    #[must_use]
    pub fn file_fingerprint_before(&self) -> &str {
        self.first_change().file_fingerprint_before()
    }

    /// Returns the first projected stable non-cryptographic fingerprint after replacement.
    #[must_use]
    pub fn file_fingerprint_after(&self) -> &str {
        self.first_change().file_fingerprint_after()
    }
}

fn validate_workspace_patch_changes(
    changes: &[WorkspacePatchChangeEvidence],
) -> Result<(), ActionProposalError> {
    if changes.is_empty() {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "changes",
            reason: "must contain at least one file change",
        });
    }
    Ok(())
}

fn validate_workspace_patch_counts(
    preimage_bytes: usize,
    replacement_bytes: usize,
    file_bytes_before: usize,
    file_bytes_after: usize,
) -> Result<(), ActionProposalError> {
    if preimage_bytes == 0 && file_bytes_before != 0 {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "preimage_bytes",
            reason: "must be greater than zero unless the file is new",
        });
    }

    let expected_after = file_bytes_before
        .checked_sub(preimage_bytes)
        .and_then(|unchanged| unchanged.checked_add(replacement_bytes))
        .ok_or(ActionProposalError::InvalidWorkspacePatch {
            field: "file_bytes_after",
            reason: "must be consistent with before, preimage, and replacement byte counts",
        })?;
    if expected_after != file_bytes_after {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "file_bytes_after",
            reason: "must equal file_bytes_before - preimage_bytes + replacement_bytes",
        });
    }

    Ok(())
}

fn validate_workspace_patch_fingerprint(
    field: &'static str,
    value: String,
) -> Result<String, ActionProposalError> {
    if value.trim().is_empty() {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field,
            reason: "must not be blank",
        });
    }
    if value.len() > 128 {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field,
            reason: "exceeds the byte limit",
        });
    }
    let Some((algorithm, digest)) = value.split_once(':') else {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field,
            reason: "must include an algorithm prefix",
        });
    };
    if algorithm != "fnv1a64" {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field,
            reason: "must use the fnv1a64 fingerprint prefix",
        });
    }
    if digest.len() != 16 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field,
            reason: "must include 16 hexadecimal digest characters",
        });
    }

    Ok(value)
}

/// Validation errors for runtime-owned action proposal values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ActionProposalError {
    /// Read-only actions do not need mutating action proposals.
    #[error("read-only actions do not need action proposals")]
    ReadOnlyAction,

    /// A compact proposal text field was invalid.
    #[error("action proposal {field} {reason}")]
    InvalidText {
        /// Invalid field name.
        field: &'static str,
        /// Validation failure detail.
        reason: &'static str,
    },

    /// Workspace patch proposal metadata was invalid.
    #[error("workspace patch proposal {field} {reason}")]
    InvalidWorkspacePatch {
        /// Invalid field name.
        field: &'static str,
        /// Validation failure detail.
        reason: &'static str,
    },

    /// Proposal evidence did not match the proposal action kind.
    #[error("action proposal evidence does not match action kind {action_kind:?}")]
    EvidenceActionKindMismatch {
        /// Action kind supplied for the proposal.
        action_kind: ToolActionKind,
    },

    /// Process action proposal metadata was invalid.
    #[error("process action proposal metadata invalid: {source}")]
    InvalidProcessAction {
        /// Source process action validation error.
        #[from]
        source: ProcessActionError,
    },
}

const MAX_ACTION_PROPOSAL_TEXT_BYTES: usize = 512;
const MAX_WORKSPACE_PATCH_RELATIVE_PATH_BYTES: usize = 4096;

fn validate_compact_proposal_text(
    field: &'static str,
    value: String,
) -> Result<String, ActionProposalError> {
    if value.trim().is_empty() {
        return Err(ActionProposalError::InvalidText {
            field,
            reason: "must not be blank",
        });
    }
    if value.len() > MAX_ACTION_PROPOSAL_TEXT_BYTES {
        return Err(ActionProposalError::InvalidText {
            field,
            reason: "exceeds the byte limit",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ActionProposalError::InvalidText {
            field,
            reason: "must not contain control characters",
        });
    }
    Ok(value)
}

fn validate_workspace_patch_relative_path(value: String) -> Result<String, ActionProposalError> {
    if value.trim().is_empty() {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "must not be blank",
        });
    }
    if value.len() > MAX_WORKSPACE_PATCH_RELATIVE_PATH_BYTES {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "exceeds the byte limit",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "must not contain control characters",
        });
    }
    if value.split('/').any(str::is_empty) {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "must not contain empty path segments",
        });
    }

    let path = Path::new(&value);
    if path.is_absolute() {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "must be relative",
        });
    }

    let mut saw_component = false;
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                if value.to_str().is_none() {
                    return Err(ActionProposalError::InvalidWorkspacePatch {
                        field: "relative_path",
                        reason: "components must be UTF-8",
                    });
                }
                saw_component = true;
            }
            Component::CurDir | Component::ParentDir => {
                return Err(ActionProposalError::InvalidWorkspacePatch {
                    field: "relative_path",
                    reason: "must not contain dot segments",
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ActionProposalError::InvalidWorkspacePatch {
                    field: "relative_path",
                    reason: "must be relative",
                });
            }
        }
    }

    if !saw_component {
        return Err(ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            reason: "must name a file",
        });
    }

    Ok(value)
}

fn validate_proposal_evidence_matches_action_kind(
    action_kind: ToolActionKind,
    evidence: &ActionProposalEvidence,
) -> Result<(), ActionProposalError> {
    if evidence.matches_action_kind(action_kind) {
        Ok(())
    } else {
        Err(ActionProposalError::EvidenceActionKindMismatch { action_kind })
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
    action_kind: ToolActionKind,
    proposals_enabled: bool,
    runner: ToolRunner,
    concurrency: ToolConcurrency,
}

impl RegisteredTool {
    /// Creates a registered tool with an explicit runtime-owned action category.
    ///
    /// The spec is provider-visible after adapter rendering, but the executor
    /// remains a runtime-owned boundary. The action category stays inside
    /// runtime and is not rendered into the provider-visible tool spec.
    #[must_use]
    pub fn new(
        spec: ToolSpec,
        executor: Arc<dyn ToolExecutor>,
        action_kind: ToolActionKind,
    ) -> Self {
        Self {
            spec,
            executor,
            action_kind,
            proposals_enabled: false,
            runner: ToolRunner::Runtime,
            concurrency: ToolConcurrency::Exclusive,
        }
    }

    /// Enables read-only proposal evidence for a mutating tool.
    ///
    /// Runtime calls [`ToolExecutor::propose`] only for registered tools that
    /// explicitly opt in here. The hook is still skipped for read-only tools.
    #[must_use]
    pub fn with_action_proposal(mut self) -> Self {
        self.proposals_enabled = true;
        self
    }

    /// Opts this tool into bounded concurrent execution within one model batch.
    ///
    /// Use this only when concurrent calls cannot race through writes,
    /// processes, network access, permissions, or runtime-control state.
    #[must_use]
    pub fn with_parallel_safe_execution(mut self) -> Self {
        self.concurrency = ToolConcurrency::ParallelSafe;
        self
    }

    /// Creates a registered read-only tool.
    ///
    /// Use this only for tools that do not write workspace state or execute
    /// commands. Network access is controlled by runtime profile, not by this
    /// constructor.
    #[must_use]
    pub fn read_only(spec: ToolSpec, executor: Arc<dyn ToolExecutor>) -> Self {
        Self::new(spec, executor, ToolActionKind::ReadOnly)
    }

    /// Creates a tool whose execution is delegated to an external SDK bridge.
    #[must_use]
    pub fn bridge(spec: ToolSpec) -> Self {
        Self {
            spec,
            executor: Arc::new(BridgeExecutor),
            action_kind: ToolActionKind::ReadOnly,
            proposals_enabled: false,
            runner: ToolRunner::Bridge,
            concurrency: ToolConcurrency::Exclusive,
        }
    }

    /// Borrows the provider-visible tool specification.
    #[must_use]
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Returns the runtime-owned action category for this tool.
    #[must_use]
    pub fn action_kind(&self) -> ToolActionKind {
        self.action_kind
    }

    /// Returns where this tool is executed.
    #[must_use]
    pub fn runner(&self) -> ToolRunner {
        self.runner
    }

    /// Returns the runtime execution policy for batched calls.
    #[must_use]
    pub fn concurrency(&self) -> ToolConcurrency {
        self.concurrency
    }

    /// Returns whether this tool opted into action proposal evidence.
    #[must_use]
    pub fn proposals_enabled(&self) -> bool {
        self.proposals_enabled
    }

    pub(crate) fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::clone(&self.executor)
    }
}

struct BridgeExecutor;

impl ToolExecutor for BridgeExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            Err(ToolExecutionError::infrastructure(format!(
                "bridge tool {} must be executed by a bridge runner",
                call.name().as_str()
            )))
        })
    }
}

impl std::fmt::Debug for RegisteredTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredTool")
            .field("spec", &self.spec)
            .field("action_kind", &self.action_kind)
            .field("proposals_enabled", &self.proposals_enabled)
            .field("runner", &self.runner)
            .field("concurrency", &self.concurrency)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolRegistry {
    tools: BTreeMap<ToolName, RegisteredToolEntry>,
    order: Vec<ToolName>,
}

#[derive(Debug, Clone)]
struct RegisteredToolEntry {
    tool: RegisteredTool,
    input_validator: CompiledToolInputValidator,
}

impl ToolRegistry {
    pub(crate) fn from_registered(tools: Vec<RegisteredTool>) -> Result<Self, ToolRegistryError> {
        let mut registry = BTreeMap::new();
        let mut order = Vec::with_capacity(tools.len());

        for tool in tools {
            let name = tool.spec().name().clone();
            let input_validator = CompiledToolInputValidator::compile(tool.spec().input_schema())
                .map_err(|source| ToolRegistryError::InvalidToolInputSchema {
                name: name.clone(),
                message: source.to_string(),
            })?;
            let entry = RegisteredToolEntry {
                tool,
                input_validator,
            };
            if registry.insert(name.clone(), entry).is_some() {
                return Err(ToolRegistryError::DuplicateName { name });
            }
            order.push(name);
        }

        Ok(Self {
            tools: registry,
            order,
        })
    }

    pub(crate) fn tool_specs(&self) -> Vec<ToolSpec> {
        self.order
            .iter()
            .filter_map(|name| self.tools.get(name))
            .map(|entry| entry.tool.spec().clone())
            .collect()
    }

    pub(crate) fn registered_tool(&self, name: &ToolName) -> Option<&RegisteredTool> {
        self.tools.get(name).map(|entry| &entry.tool)
    }

    pub(crate) fn validate_tool_input(
        &self,
        call: &PendingToolCall,
    ) -> Option<Result<(), ToolInputValidationError>> {
        self.tools
            .get(call.name())
            .map(|entry| entry.input_validator.validate_call(call))
    }

    pub(crate) fn first_bridge_tool_name(&self) -> Option<&ToolName> {
        self.tools
            .iter()
            .find_map(|(name, entry)| (entry.tool.runner() == ToolRunner::Bridge).then_some(name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolRegistryError {
    DuplicateName { name: ToolName },
    InvalidToolInputSchema { name: ToolName, message: String },
}

#[cfg(test)]
mod tests {
    use super::{
        ActionProposal, ActionProposalError, ActionProposalEvidence, RegisteredTool,
        ToolActionKind, ToolConcurrency, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor,
        ToolExecutorFuture, ToolRunner, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
    };
    use crate::{ProcessActionIntent, ProcessEnvPolicy};
    use merry_core::{
        PendingToolCall, ToolCallArguments, ToolCallId, ToolInputSchema, ToolName, ToolSpec,
    };
    use schemars::Schema;
    use serde_json::json;
    use std::sync::Arc;

    struct StaticToolExecutor;

    impl ToolExecutor for StaticToolExecutor {
        fn execute<'a>(
            &'a self,
            _call: PendingToolCall,
            _context: ToolExecutionContext,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async { Ok(ToolExecutionOutcome::succeeded_text("ok")) })
        }
    }

    fn tool_spec(name: &str) -> ToolSpec {
        let schema = Schema::try_from(json!({ "type": "object" }))
            .expect("test schema should be a JSON schema");
        ToolSpec::new(
            ToolName::new(name).expect("valid tool name"),
            "Test tool",
            ToolInputSchema::new(schema).expect("valid tool schema"),
        )
        .expect("valid tool spec")
    }

    fn pending_call(name: &str) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("call-proposal").expect("valid call id"),
            ToolName::new(name).expect("valid tool name"),
            ToolCallArguments::new(Default::default()),
        )
    }

    #[test]
    fn read_only_constructor_classifies_tool_as_read_only() {
        let tool =
            RegisteredTool::read_only(tool_spec("read_only_tool"), Arc::new(StaticToolExecutor));

        assert_eq!(tool.action_kind(), ToolActionKind::ReadOnly);
    }

    #[test]
    fn registered_tool_defaults_to_runtime_runner() {
        let tool =
            RegisteredTool::read_only(tool_spec("lookup_order"), Arc::new(StaticToolExecutor));

        assert_eq!(tool.runner(), ToolRunner::Runtime);
    }

    #[test]
    fn bridge_tool_carries_spec_without_runtime_executor() {
        let tool = RegisteredTool::bridge(tool_spec("lookup_order"));

        assert_eq!(tool.runner(), ToolRunner::Bridge);
        assert_eq!(tool.spec().name().as_str(), "lookup_order");
    }

    #[test]
    fn registered_tools_default_to_exclusive_execution() {
        let explicit = RegisteredTool::new(
            tool_spec("write_tool"),
            Arc::new(StaticToolExecutor),
            ToolActionKind::WorkspaceWrite,
        );
        let read_only =
            RegisteredTool::read_only(tool_spec("read_tool"), Arc::new(StaticToolExecutor));
        let bridge = RegisteredTool::bridge(tool_spec("bridge_tool"));

        assert_eq!(explicit.concurrency(), ToolConcurrency::Exclusive);
        assert_eq!(read_only.concurrency(), ToolConcurrency::Exclusive);
        assert_eq!(bridge.concurrency(), ToolConcurrency::Exclusive);
    }

    #[test]
    fn parallel_safe_opt_in_changes_only_runtime_metadata() {
        let tool =
            RegisteredTool::read_only(tool_spec("lookup_order"), Arc::new(StaticToolExecutor));
        let visible_spec = serde_json::to_value(tool.spec()).expect("tool spec should serialize");
        let tool = tool.with_parallel_safe_execution();

        assert_eq!(tool.concurrency(), ToolConcurrency::ParallelSafe);
        assert_eq!(
            serde_json::to_value(tool.spec()).expect("tool spec should serialize"),
            visible_spec
        );
    }

    #[test]
    fn explicit_constructor_preserves_non_read_action_kind() {
        let tool = RegisteredTool::new(
            tool_spec("write_tool"),
            Arc::new(StaticToolExecutor),
            ToolActionKind::WorkspaceWrite,
        );

        assert_eq!(tool.action_kind(), ToolActionKind::WorkspaceWrite);
        assert!(!tool.proposals_enabled());
    }

    #[test]
    fn action_proposal_opt_in_marks_registered_tool() {
        let tool = RegisteredTool::new(
            tool_spec("write_tool"),
            Arc::new(StaticToolExecutor),
            ToolActionKind::WorkspaceWrite,
        )
        .with_action_proposal();

        assert_eq!(tool.action_kind(), ToolActionKind::WorkspaceWrite);
        assert!(tool.proposals_enabled());
    }

    #[test]
    fn workspace_patch_proposal_validates_relative_path_and_sizes() {
        let proposal = WorkspacePatchProposal::new(
            "dir/note.txt",
            3,
            5,
            11,
            13,
            "fnv1a64:0123456789abcdef",
            "fnv1a64:fedcba9876543210",
        )
        .expect("valid workspace patch proposal");

        assert_eq!(proposal.relative_path(), "dir/note.txt");
        assert_eq!(proposal.preimage_bytes(), 3);
        assert_eq!(proposal.replacement_bytes(), 5);
        assert_eq!(proposal.file_bytes_before(), 11);
        assert_eq!(proposal.file_bytes_after(), 13);
        assert_eq!(
            proposal.file_fingerprint_before(),
            "fnv1a64:0123456789abcdef"
        );
        assert_eq!(
            proposal.file_fingerprint_after(),
            "fnv1a64:fedcba9876543210"
        );

        let new_file = WorkspacePatchProposal::new(
            "new.txt",
            0,
            5,
            0,
            5,
            "fnv1a64:0123456789abcdef",
            "fnv1a64:fedcba9876543210",
        )
        .expect("new-file workspace patch proposal should allow an empty preimage");
        assert_eq!(new_file.preimage_bytes(), 0);
        assert_eq!(new_file.file_bytes_before(), 0);
        assert_eq!(new_file.file_bytes_after(), 5);

        let absolute = WorkspacePatchProposal::new(
            "/tmp/note.txt",
            3,
            5,
            11,
            13,
            "fnv1a64:0123456789abcdef",
            "fnv1a64:fedcba9876543210",
        )
        .expect_err("absolute paths are rejected");
        assert!(matches!(
            absolute,
            ActionProposalError::InvalidWorkspacePatch {
                field: "relative_path",
                ..
            }
        ));

        let dot_segment = WorkspacePatchProposal::new(
            "dir/../note.txt",
            3,
            5,
            11,
            13,
            "fnv1a64:0123456789abcdef",
            "fnv1a64:fedcba9876543210",
        )
        .expect_err("dot segments are rejected");
        assert!(matches!(
            dot_segment,
            ActionProposalError::InvalidWorkspacePatch {
                field: "relative_path",
                ..
            }
        ));

        let mismatched = WorkspacePatchProposal::new(
            "dir/note.txt",
            3,
            5,
            11,
            99,
            "fnv1a64:0123456789abcdef",
            "fnv1a64:fedcba9876543210",
        )
        .expect_err("projected size must match patch sizes");
        assert!(matches!(
            mismatched,
            ActionProposalError::InvalidWorkspacePatch {
                field: "file_bytes_after",
                ..
            }
        ));
    }

    #[test]
    fn workspace_patch_execution_evidence_validates_counts_and_fingerprints() {
        let evidence = WorkspacePatchExecutionEvidence::new(
            "dir/note.txt",
            3,
            5,
            11,
            13,
            "fnv1a64:0123456789abcdef",
            "fnv1a64:fedcba9876543210",
        )
        .expect("valid workspace patch execution evidence");

        assert_eq!(evidence.relative_path(), "dir/note.txt");
        assert_eq!(evidence.preimage_bytes(), 3);
        assert_eq!(evidence.replacement_bytes(), 5);
        assert_eq!(evidence.file_bytes_before(), 11);
        assert_eq!(evidence.file_bytes_after(), 13);
        assert_eq!(
            evidence.file_fingerprint_before(),
            "fnv1a64:0123456789abcdef"
        );
        assert_eq!(
            evidence.file_fingerprint_after(),
            "fnv1a64:fedcba9876543210"
        );

        let invalid = WorkspacePatchExecutionEvidence::new(
            "dir/note.txt",
            3,
            5,
            11,
            13,
            "sha256:not-accepted",
            "fnv1a64:fedcba9876543210",
        )
        .expect_err("fingerprints use the explicit non-cryptographic prefix");
        assert!(matches!(
            invalid,
            ActionProposalError::InvalidWorkspacePatch {
                field: "file_fingerprint_before",
                ..
            }
        ));
    }

    #[test]
    fn action_proposal_rejects_read_only_and_blank_text() {
        let call = pending_call("workspace_patch");
        let evidence = ActionProposalEvidence::WorkspacePatch(
            WorkspacePatchProposal::new(
                "note.txt",
                3,
                5,
                11,
                13,
                "fnv1a64:0123456789abcdef",
                "fnv1a64:fedcba9876543210",
            )
            .expect("valid workspace patch proposal"),
        );

        let read_only = ActionProposal::new(
            &call,
            ToolActionKind::ReadOnly,
            "label",
            "note.txt",
            "summary",
            evidence.clone(),
        )
        .expect_err("read-only actions do not need proposals");
        assert!(matches!(read_only, ActionProposalError::ReadOnlyAction));

        let blank = ActionProposal::new(
            &call,
            ToolActionKind::WorkspaceWrite,
            " ",
            "note.txt",
            "summary",
            evidence,
        )
        .expect_err("blank label should be rejected");
        assert!(matches!(
            blank,
            ActionProposalError::InvalidText { field: "label", .. }
        ));
    }

    #[test]
    fn action_proposal_evidence_must_match_action_kind() {
        let call = pending_call("run_command");
        let process_intent = ProcessActionIntent::new(
            vec!["cargo".to_owned(), "test".to_owned()],
            Some("crates/merry-runtime".to_owned()),
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("valid process intent");
        let process_proposal = ActionProposal::new(
            &call,
            ToolActionKind::CommandExec,
            "process",
            "cargo test",
            "Run cargo test in the runtime crate",
            ActionProposalEvidence::ProcessAction(process_intent),
        )
        .expect("process evidence matches command exec action");
        assert!(matches!(
            process_proposal.evidence(),
            ActionProposalEvidence::ProcessAction(_)
        ));

        let patch = WorkspacePatchProposal::new(
            "note.txt",
            3,
            5,
            11,
            13,
            "fnv1a64:0123456789abcdef",
            "fnv1a64:fedcba9876543210",
        )
        .expect("valid workspace patch proposal");
        let mismatched = ActionProposal::new(
            &call,
            ToolActionKind::CommandExec,
            "workspace patch",
            "note.txt",
            "Patch evidence cannot stand in for command execution",
            ActionProposalEvidence::WorkspacePatch(patch),
        )
        .expect_err("workspace patch evidence must not match command exec");
        assert!(matches!(
            mismatched,
            ActionProposalError::EvidenceActionKindMismatch {
                action_kind: ToolActionKind::CommandExec
            }
        ));
    }
}
