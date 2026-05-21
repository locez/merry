//! Runtime-owned action policy taxonomy and default hard policy.
//!
//! These types describe runtime policy decisions for registered tool actions.
//! They are intentionally separate from provider-visible tool specs and provider
//! wire formats.

use crate::{ActionProposal, ActionProposalEvidence, ToolActionKind};

/// Runtime-owned risk tier for a registered tool action.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionRiskTier {
    /// Reads runtime or workspace state without changing it.
    ReadOnly,
    /// Performs bounded, low-risk edits.
    EditLow,
    /// Performs broader or higher-impact edits.
    EditElevated,
    /// Starts a bounded, low-risk local process.
    ProcessLow,
    /// Starts a higher-risk local process.
    ProcessHigh,
    /// Uses network access.
    Network,
    /// Is forbidden by runtime policy.
    Forbidden,
}

/// Runtime-owned disposition for an action policy decision.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionPolicyDisposition {
    /// The action may execute without additional gates.
    Allow,
    /// The action is blocked by hard policy.
    Deny,
    /// The action requires explicit approval before execution.
    NeedsApproval,
    /// The action requires deterministic or model-backed review before execution.
    NeedsReview,
}

/// Runtime-owned decision for a registered tool action.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionPolicyDecision {
    action_kind: ToolActionKind,
    risk_tier: ActionRiskTier,
    disposition: ActionPolicyDisposition,
    reason: &'static str,
}

impl ActionPolicyDecision {
    /// Creates a runtime-owned action policy decision.
    #[must_use]
    const fn new(
        action_kind: ToolActionKind,
        risk_tier: ActionRiskTier,
        disposition: ActionPolicyDisposition,
        reason: &'static str,
    ) -> Self {
        Self {
            action_kind,
            risk_tier,
            disposition,
            reason,
        }
    }

    /// Returns the action category considered by policy.
    #[must_use]
    pub(crate) const fn action_kind(&self) -> ToolActionKind {
        self.action_kind
    }

    /// Returns the risk tier assigned by policy.
    #[must_use]
    pub(crate) const fn risk_tier(&self) -> ActionRiskTier {
        self.risk_tier
    }

    /// Returns the policy disposition.
    #[must_use]
    pub(crate) const fn disposition(&self) -> ActionPolicyDisposition {
        self.disposition
    }

    /// Returns the compact hard-policy reason.
    #[must_use]
    pub(crate) const fn reason(&self) -> &'static str {
        self.reason
    }

    pub(crate) const fn is_allowed(&self) -> bool {
        matches!(self.disposition, ActionPolicyDisposition::Allow)
    }

    /// Returns the same policy disposition with a proposal-aware risk tier.
    #[must_use]
    pub(crate) const fn with_risk_tier(&self, risk_tier: ActionRiskTier) -> Self {
        Self {
            action_kind: self.action_kind,
            risk_tier,
            disposition: self.disposition,
            reason: self.reason,
        }
    }
}

/// Classifies the runtime-owned risk tier for a tool action.
#[must_use]
pub(crate) fn classify_tool_action_risk(
    action_kind: ToolActionKind,
    proposal: Option<&ActionProposal>,
) -> ActionRiskTier {
    match action_kind {
        ToolActionKind::ReadOnly => ActionRiskTier::ReadOnly,
        ToolActionKind::WorkspaceWrite => {
            if matches!(
                proposal.map(ActionProposal::evidence),
                Some(ActionProposalEvidence::WorkspacePatch(_))
            ) {
                ActionRiskTier::EditLow
            } else {
                ActionRiskTier::EditElevated
            }
        }
        ToolActionKind::CommandExec => ActionRiskTier::ProcessHigh,
        ToolActionKind::Network => ActionRiskTier::Forbidden,
    }
}

/// Default runtime hard policy for registered tool actions.
///
/// The MVP policy is intentionally conservative and equivalent to the original
/// hard-coded behavior: read-only actions are allowed and all mutating,
/// process, or network actions are denied.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DefaultActionPolicy;

impl DefaultActionPolicy {
    /// Decides whether a registered tool action may execute.
    #[must_use]
    pub(crate) fn decide(&self, action_kind: ToolActionKind) -> ActionPolicyDecision {
        match action_kind {
            ToolActionKind::ReadOnly => ActionPolicyDecision::new(
                ToolActionKind::ReadOnly,
                classify_tool_action_risk(ToolActionKind::ReadOnly, None),
                ActionPolicyDisposition::Allow,
                "read-only tool actions are allowed by default policy",
            ),
            ToolActionKind::WorkspaceWrite => ActionPolicyDecision::new(
                ToolActionKind::WorkspaceWrite,
                classify_tool_action_risk(ToolActionKind::WorkspaceWrite, None),
                ActionPolicyDisposition::Deny,
                "workspace write tool actions are denied by default policy",
            ),
            ToolActionKind::CommandExec => ActionPolicyDecision::new(
                ToolActionKind::CommandExec,
                classify_tool_action_risk(ToolActionKind::CommandExec, None),
                ActionPolicyDisposition::Deny,
                "command execution tool actions are denied by default policy",
            ),
            ToolActionKind::Network => ActionPolicyDecision::new(
                ToolActionKind::Network,
                classify_tool_action_risk(ToolActionKind::Network, None),
                ActionPolicyDisposition::Deny,
                "network tool actions are denied by default policy",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActionPolicyDisposition, ActionRiskTier, DefaultActionPolicy, classify_tool_action_risk,
    };
    use crate::{ActionProposal, ActionProposalEvidence, ToolActionKind, WorkspacePatchProposal};
    use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
    use serde_json::json;

    fn pending_tool_call() -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("call-risk-classifier").expect("valid call id"),
            ToolName::new("patch_file").expect("valid tool name"),
            ToolCallArguments::try_from(json!({ "path": "notes/proposed.txt" }))
                .expect("object arguments are valid"),
        )
    }

    fn workspace_patch_proposal(call: &PendingToolCall) -> ActionProposal {
        let patch = WorkspacePatchProposal::new("notes/proposed.txt", 3, 7, 20, 24)
            .expect("test patch proposal is valid");
        ActionProposal::new(
            call,
            ToolActionKind::WorkspaceWrite,
            "workspace patch",
            "notes/proposed.txt",
            "Replace one matched preimage in notes/proposed.txt",
            ActionProposalEvidence::WorkspacePatch(patch),
        )
        .expect("test action proposal is valid")
    }

    #[test]
    fn classifier_assigns_read_only_risk() {
        assert_eq!(
            classify_tool_action_risk(ToolActionKind::ReadOnly, None),
            ActionRiskTier::ReadOnly
        );
    }

    #[test]
    fn classifier_assigns_low_edit_risk_for_workspace_patch_proposal() {
        let call = pending_tool_call();
        let proposal = workspace_patch_proposal(&call);

        assert_eq!(
            classify_tool_action_risk(ToolActionKind::WorkspaceWrite, Some(&proposal)),
            ActionRiskTier::EditLow
        );
    }

    #[test]
    fn classifier_assigns_elevated_edit_risk_without_compatible_proposal() {
        assert_eq!(
            classify_tool_action_risk(ToolActionKind::WorkspaceWrite, None),
            ActionRiskTier::EditElevated
        );
    }

    #[test]
    fn classifier_assigns_high_process_risk_for_command_exec() {
        assert_eq!(
            classify_tool_action_risk(ToolActionKind::CommandExec, None),
            ActionRiskTier::ProcessHigh
        );
    }

    #[test]
    fn network_actions_are_forbidden_and_denied_by_default_policy() {
        assert_eq!(
            classify_tool_action_risk(ToolActionKind::Network, None),
            ActionRiskTier::Forbidden
        );

        let decision = DefaultActionPolicy.decide(ToolActionKind::Network);
        assert_eq!(decision.action_kind(), ToolActionKind::Network);
        assert_eq!(decision.risk_tier(), ActionRiskTier::Forbidden);
        assert_eq!(decision.disposition(), ActionPolicyDisposition::Deny);
        assert!(!decision.is_allowed());
    }
}
