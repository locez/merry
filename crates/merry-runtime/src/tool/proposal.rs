use crate::{
    ProcessActionError, ProcessActionIntent, ProcessExecutionEvidence,
    tool::patch_evidence::{WorkspacePatchExecutionEvidence, WorkspacePatchProposal},
};
use merry_core::{PendingToolCall, ToolCallId, ToolName};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Runtime-owned action category for registered tools.
///
/// This metadata is intentionally not part of provider-visible [`merry_core::ToolSpec`].
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
    pub(super) tool_call_id: ToolCallId,
    pub(super) tool_name: ToolName,
    pub(super) action_kind: ToolActionKind,
    pub(super) label: String,
    pub(super) subject: String,
    pub(super) summary: String,
    pub(super) evidence: ActionProposalEvidence,
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
    pub(super) fn matches_action_kind(&self, action_kind: ToolActionKind) -> bool {
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

pub(super) const MAX_ACTION_PROPOSAL_TEXT_BYTES: usize = 512;

pub(super) fn validate_compact_proposal_text(
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

pub(super) fn validate_proposal_evidence_matches_action_kind(
    action_kind: ToolActionKind,
    evidence: &ActionProposalEvidence,
) -> Result<(), ActionProposalError> {
    if evidence.matches_action_kind(action_kind) {
        Ok(())
    } else {
        Err(ActionProposalError::EvidenceActionKindMismatch { action_kind })
    }
}
