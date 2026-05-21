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

use crate::ArtifactContent;
use merry_core::{
    ErrorInfo, PendingToolCall, ToolCallId, ToolCallResultStatus, ToolName, ToolSpec,
};
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
/// `Ok(None)` means the executor cannot provide deterministic proposal evidence
/// for this call and policy should continue with its normal decision path.
pub type ToolActionProposalResult = Result<Option<ActionProposal>, ToolExecutionError>;

/// Context passed to a tool executor.
///
/// The context is intentionally small for the MVP: cancellation is cooperative
/// and runtime state mutation stays owned by [`crate::Runtime::execute_tool_call`].
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
    ///
    /// Executors should check this token at cancellation points and return
    /// [`ToolExecutionError::Cancelled`] when no durable result was produced.
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
        Box::pin(async { Ok(None) })
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolActionKind {
    /// Reads runtime or workspace state without changing it.
    ReadOnly,
    /// Writes files or other state in the configured workspace.
    WorkspaceWrite,
    /// Executes a local command or process.
    CommandExec,
    /// Uses network access.
    Network,
}

impl ToolActionKind {
    /// Returns whether this action category can cause external side effects.
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
/// not part of `merry_core::RuntimeEvent`, is not provider wire format, and must
/// not be rendered into provider-visible tool specs or continuations.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        Ok(())
    }
}

/// Provider-neutral deterministic evidence attached to an action proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionProposalEvidence {
    /// A constrained single-file workspace patch proposal.
    WorkspacePatch(WorkspacePatchProposal),
}

/// Deterministic metadata for a constrained workspace patch proposal.
///
/// This stores only relative workspace identity and byte counts needed for a
/// future edit decision. It does not store host absolute paths or provider wire
/// data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePatchProposal {
    relative_path: String,
    preimage_bytes: usize,
    replacement_bytes: usize,
    file_bytes_before: usize,
    file_bytes_after: usize,
}

impl WorkspacePatchProposal {
    /// Creates validated metadata for a single-preimage workspace patch.
    pub fn new(
        relative_path: impl Into<String>,
        preimage_bytes: usize,
        replacement_bytes: usize,
        file_bytes_before: usize,
        file_bytes_after: usize,
    ) -> Result<Self, ActionProposalError> {
        let relative_path = validate_workspace_patch_relative_path(relative_path.into())?;
        if preimage_bytes == 0 {
            return Err(ActionProposalError::InvalidWorkspacePatch {
                field: "preimage_bytes",
                reason: "must be greater than zero",
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

        Ok(Self {
            relative_path,
            preimage_bytes,
            replacement_bytes,
            file_bytes_before,
            file_bytes_after,
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

    /// Returns the file size before replacement.
    #[must_use]
    pub fn file_bytes_before(&self) -> usize {
        self.file_bytes_before
    }

    /// Returns the projected file size after replacement.
    #[must_use]
    pub fn file_bytes_after(&self) -> usize {
        self.file_bytes_after
    }
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

    /// Creates a registered read-only tool.
    ///
    /// Use this only for tools that do not write workspace state, execute
    /// commands, or access the network.
    #[must_use]
    pub fn read_only(spec: ToolSpec, executor: Arc<dyn ToolExecutor>) -> Self {
        Self::new(spec, executor, ToolActionKind::ReadOnly)
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

    /// Returns whether this tool opted into action proposal evidence.
    #[must_use]
    pub fn proposals_enabled(&self) -> bool {
        self.proposals_enabled
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
            .field("action_kind", &self.action_kind)
            .field("proposals_enabled", &self.proposals_enabled)
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

    pub(crate) fn registered_tool(&self, name: &ToolName) -> Option<&RegisteredTool> {
        self.tools.get(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DuplicateToolName {
    pub(crate) name: ToolName,
}

#[cfg(test)]
mod tests {
    use super::{
        ActionProposal, ActionProposalError, ActionProposalEvidence, RegisteredTool,
        ToolActionKind, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor,
        ToolExecutorFuture, WorkspacePatchProposal,
    };
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
        let proposal = WorkspacePatchProposal::new("dir/note.txt", 3, 5, 11, 13)
            .expect("valid workspace patch proposal");

        assert_eq!(proposal.relative_path(), "dir/note.txt");
        assert_eq!(proposal.preimage_bytes(), 3);
        assert_eq!(proposal.replacement_bytes(), 5);
        assert_eq!(proposal.file_bytes_before(), 11);
        assert_eq!(proposal.file_bytes_after(), 13);

        let absolute = WorkspacePatchProposal::new("/tmp/note.txt", 3, 5, 11, 13)
            .expect_err("absolute paths are rejected");
        assert!(matches!(
            absolute,
            ActionProposalError::InvalidWorkspacePatch {
                field: "relative_path",
                ..
            }
        ));

        let dot_segment = WorkspacePatchProposal::new("dir/../note.txt", 3, 5, 11, 13)
            .expect_err("dot segments are rejected");
        assert!(matches!(
            dot_segment,
            ActionProposalError::InvalidWorkspacePatch {
                field: "relative_path",
                ..
            }
        ));

        let mismatched = WorkspacePatchProposal::new("dir/note.txt", 3, 5, 11, 99)
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
    fn action_proposal_rejects_read_only_and_blank_text() {
        let call = pending_call("workspace_patch_file");
        let evidence = ActionProposalEvidence::WorkspacePatch(
            WorkspacePatchProposal::new("note.txt", 3, 5, 11, 13)
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
}
