//! Runtime-owned action policy taxonomy and default hard policy.
//!
//! These types describe runtime policy decisions for registered tool actions.
//! They are intentionally separate from provider-visible tool specs and provider
//! wire formats.

use crate::ToolActionKind;

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
    #[cfg(test)]
    pub(crate) const fn action_kind(&self) -> ToolActionKind {
        self.action_kind
    }

    /// Returns the risk tier assigned by policy.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn risk_tier(&self) -> ActionRiskTier {
        self.risk_tier
    }

    /// Returns the policy disposition.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn disposition(&self) -> ActionPolicyDisposition {
        self.disposition
    }

    pub(crate) const fn is_allowed(&self) -> bool {
        matches!(self.disposition, ActionPolicyDisposition::Allow)
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
    pub(crate) const fn decide(&self, action_kind: ToolActionKind) -> ActionPolicyDecision {
        match action_kind {
            ToolActionKind::ReadOnly => ActionPolicyDecision::new(
                ToolActionKind::ReadOnly,
                ActionRiskTier::ReadOnly,
                ActionPolicyDisposition::Allow,
                "read-only tool actions are allowed by default policy",
            ),
            ToolActionKind::WorkspaceWrite => ActionPolicyDecision::new(
                ToolActionKind::WorkspaceWrite,
                ActionRiskTier::EditElevated,
                ActionPolicyDisposition::Deny,
                "workspace write tool actions are denied by default policy",
            ),
            ToolActionKind::CommandExec => ActionPolicyDecision::new(
                ToolActionKind::CommandExec,
                ActionRiskTier::ProcessHigh,
                ActionPolicyDisposition::Deny,
                "command execution tool actions are denied by default policy",
            ),
            ToolActionKind::Network => ActionPolicyDecision::new(
                ToolActionKind::Network,
                ActionRiskTier::Network,
                ActionPolicyDisposition::Deny,
                "network tool actions are denied by default policy",
            ),
        }
    }
}
