//! Runtime-owned action policy taxonomy and default hard policy.
//!
//! These types describe runtime policy decisions for registered tool actions.
//! They are intentionally separate from provider-visible tool specs and provider
//! wire formats.

use crate::{
    AcceptedLocalWorkspaceProcessAdmission, ActionProposal, ActionProposalEvidence,
    ProcessPermissionProfileId, ToolActionKind,
    process::{
        ProcessIntentClass, classify_process_intent, required_process_permission_profile_id,
    },
};
use serde::{Deserialize, Serialize};

/// Runtime-owned risk tier for a registered tool action.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActionRiskTier {
    /// Reads runtime or workspace state without changing it.
    ReadOnly,
    /// Mutates runtime-owned control state only.
    RuntimeControl,
    /// Performs bounded, low-risk edits.
    EditLow,
    /// Performs broader or higher-impact edits.
    EditElevated,
    /// Starts a bounded, low-risk local process.
    ProcessLow,
    /// Starts a local process with accepted workspace effects.
    ProcessLocalWorkspaceEffect,
    /// Starts a read-only shell wrapper under explicit shell-runner admission.
    ProcessShellReadOnly,
    /// Starts a process after explicit permission request admission.
    ProcessPermissioned,
    /// Starts a higher-risk local process.
    ProcessHigh,
    /// Uses network access.
    Network,
    /// Executes a user-configured external tool trusted by configuration.
    TrustedExternal,
    /// Is forbidden by runtime policy.
    Forbidden,
}

/// Runtime-owned disposition for an action policy decision.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

    pub(crate) fn is_fully_trusted(&self) -> bool {
        self.reason == "registered tool action is allowed by explicit fully trusted mode"
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

    /// Allows an otherwise denied action after a narrow proposal-aware opt-in.
    #[must_use]
    pub(crate) const fn allow_low_risk_workspace_patch() -> Self {
        Self::new(
            ToolActionKind::WorkspaceWrite,
            ActionRiskTier::EditLow,
            ActionPolicyDisposition::Allow,
            "low-risk workspace patch tool actions are allowed by explicit runtime opt-in",
        )
    }

    /// Allows an otherwise denied command execution after a narrow process opt-in.
    #[must_use]
    pub(crate) const fn allow_low_risk_process_action() -> Self {
        Self::new(
            ToolActionKind::CommandExec,
            ActionRiskTier::ProcessLow,
            ActionPolicyDisposition::Allow,
            "low-risk process actions are allowed by explicit runtime opt-in",
        )
    }

    /// Allows an otherwise denied local workspace effect process after explicit risk acceptance.
    #[must_use]
    pub(crate) const fn allow_accepted_local_workspace_process_action() -> Self {
        Self::new(
            ToolActionKind::CommandExec,
            ActionRiskTier::ProcessLocalWorkspaceEffect,
            ActionPolicyDisposition::Allow,
            "local workspace effect process actions are allowed only by explicit runtime opt-in for accepted local workspace process risk",
        )
    }

    /// Allows a validated process intent through the configured sandbox or
    /// explicit unrestricted host runner.
    #[must_use]
    pub(crate) const fn allow_configured_process_action() -> Self {
        Self::new(
            ToolActionKind::CommandExec,
            ActionRiskTier::ProcessHigh,
            ActionPolicyDisposition::Allow,
            "validated process actions are allowed by the configured process runner boundary",
        )
    }

    /// Denies a high-risk process after its independent action review failed
    /// or returned a denial.
    #[must_use]
    pub(crate) const fn deny_high_risk_process_action() -> Self {
        Self::new(
            ToolActionKind::CommandExec,
            ActionRiskTier::ProcessHigh,
            ActionPolicyDisposition::Deny,
            "high-risk process actions require explicit action review approval",
        )
    }

    /// Allows an otherwise denied read-only shell wrapper after explicit shell opt-in.
    #[must_use]
    pub(crate) const fn allow_read_only_shell_process_action() -> Self {
        Self::new(
            ToolActionKind::CommandExec,
            ActionRiskTier::ProcessShellReadOnly,
            ActionPolicyDisposition::Allow,
            "read-only shell process actions are allowed only by explicit runtime opt-in for a shell runner profile",
        )
    }

    /// Allows a process after explicit permission request admission.
    #[must_use]
    pub(crate) const fn allow_permissioned_process_action() -> Self {
        Self::new(
            ToolActionKind::CommandExec,
            ActionRiskTier::ProcessPermissioned,
            ActionPolicyDisposition::Allow,
            "permissioned process actions are allowed only after explicit permission admission review",
        )
    }

    /// Allows a registered tool in explicit fully trusted mode.
    pub(crate) fn allow_fully_trusted_action(action_kind: ToolActionKind) -> Self {
        let risk_tier = if action_kind == ToolActionKind::Network {
            ActionRiskTier::Network
        } else {
            classify_tool_action_risk(action_kind, None)
        };
        Self::new(
            action_kind,
            risk_tier,
            ActionPolicyDisposition::Allow,
            "registered tool action is allowed by explicit fully trusted mode",
        )
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
        ToolActionKind::RuntimeControl => ActionRiskTier::RuntimeControl,
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
        ToolActionKind::CommandExec => match proposal.map(ActionProposal::evidence) {
            Some(ActionProposalEvidence::ProcessAction(intent)) => {
                if crate::is_read_only_shell_process_action_intent(intent) {
                    return ActionRiskTier::ProcessShellReadOnly;
                }
                match classify_process_intent(intent) {
                    ProcessIntentClass::Informational => ActionRiskTier::ProcessLow,
                    ProcessIntentClass::LocalWorkspaceEffect => {
                        ActionRiskTier::ProcessLocalWorkspaceEffect
                    }
                    ProcessIntentClass::Unknown => ActionRiskTier::ProcessLocalWorkspaceEffect,
                    ProcessIntentClass::Forbidden => ActionRiskTier::ProcessHigh,
                }
            }
            _ => ActionRiskTier::ProcessHigh,
        },
        ToolActionKind::Network => ActionRiskTier::Forbidden,
        ToolActionKind::TrustedExternal => ActionRiskTier::TrustedExternal,
    }
}

/// Returns whether proposal evidence is compatible with the low-risk workspace patch lane.
#[must_use]
pub(crate) fn is_low_risk_workspace_patch_proposal(
    action_kind: ToolActionKind,
    proposal: &ActionProposal,
) -> bool {
    action_kind == ToolActionKind::WorkspaceWrite
        && proposal.action_kind() == ToolActionKind::WorkspaceWrite
        && matches!(
            proposal.evidence(),
            ActionProposalEvidence::WorkspacePatch(_)
        )
}

/// Returns whether proposal evidence is compatible with the low-risk process lane.
#[must_use]
pub(crate) fn is_low_risk_process_action_proposal(
    action_kind: ToolActionKind,
    proposal: &ActionProposal,
) -> bool {
    action_kind == ToolActionKind::CommandExec
        && proposal.action_kind() == ToolActionKind::CommandExec
        && matches!(
            proposal.evidence(),
            ActionProposalEvidence::ProcessAction(intent)
                if crate::is_low_risk_process_action_intent(intent)
        )
}

/// Returns whether proposal evidence is compatible with the read-only shell process lane.
#[must_use]
pub(crate) fn is_read_only_shell_process_action_proposal(
    action_kind: ToolActionKind,
    proposal: &ActionProposal,
) -> bool {
    action_kind == ToolActionKind::CommandExec
        && proposal.action_kind() == ToolActionKind::CommandExec
        && matches!(
            proposal.evidence(),
            ActionProposalEvidence::ProcessAction(intent)
                if crate::is_read_only_shell_process_action_intent(intent)
        )
}

/// Returns whether proposal evidence is compatible with the accepted local workspace process lane.
#[must_use]
pub(crate) fn is_local_workspace_effect_process_action_proposal(
    action_kind: ToolActionKind,
    proposal: &ActionProposal,
    admission: AcceptedLocalWorkspaceProcessAdmission,
) -> bool {
    action_kind == ToolActionKind::CommandExec
        && proposal.action_kind() == ToolActionKind::CommandExec
        && matches!(
            proposal.evidence(),
            ActionProposalEvidence::ProcessAction(intent)
                if intent.env_policy() == crate::ProcessEnvPolicy::Empty
                    && intent.stdin_text().is_none()
                    && classify_process_intent(intent)
                        == ProcessIntentClass::LocalWorkspaceEffect
                    && required_process_permission_profile_id(intent)
                        == Some(ProcessPermissionProfileId::LOCAL_WORKSPACE)
                    && admission.matches_intent(intent)
        )
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
            ToolActionKind::RuntimeControl => ActionPolicyDecision::new(
                ToolActionKind::RuntimeControl,
                classify_tool_action_risk(ToolActionKind::RuntimeControl, None),
                ActionPolicyDisposition::Allow,
                "runtime control tool actions are allowed by default policy",
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
            ToolActionKind::TrustedExternal => ActionPolicyDecision::new(
                ToolActionKind::TrustedExternal,
                classify_tool_action_risk(ToolActionKind::TrustedExternal, None),
                ActionPolicyDisposition::Allow,
                "trusted external tool actions are allowed by default policy",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActionPolicyDisposition, ActionRiskTier, DefaultActionPolicy, classify_tool_action_risk,
        is_local_workspace_effect_process_action_proposal, is_low_risk_process_action_proposal,
        is_read_only_shell_process_action_proposal,
    };
    use crate::{
        AcceptedLocalWorkspaceProcessAdmission, ActionProposal, ActionProposalEvidence,
        ProcessActionIntent, ProcessEnvPolicy, ProcessPermissionProfileId, ToolActionKind,
        WorkspacePatchProposal,
    };
    use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
    use serde_json::json;

    fn pending_tool_call() -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("call-risk-classifier").expect("valid call id"),
            ToolName::new("workspace_patch").expect("valid tool name"),
            ToolCallArguments::try_from(json!({ "patch": "*** Begin Workspace Patch\n*** Update File: notes/proposed.txt\n-old\n+new\n*** End Workspace Patch" }))
                .expect("object arguments are valid"),
        )
    }

    fn workspace_patch_proposal(call: &PendingToolCall) -> ActionProposal {
        let patch = WorkspacePatchProposal::new(
            "notes/proposed.txt",
            3,
            7,
            20,
            24,
            "fnv1a64:0000000000000001",
            "fnv1a64:0000000000000002",
        )
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

    fn process_proposal(call: &PendingToolCall, argv: &[&str]) -> ActionProposal {
        process_proposal_with_policy(call, argv, ProcessEnvPolicy::empty(), None)
    }

    fn process_proposal_with_policy(
        call: &PendingToolCall,
        argv: &[&str],
        env_policy: ProcessEnvPolicy,
        stdin_text: Option<&str>,
    ) -> ActionProposal {
        let intent = ProcessActionIntent::new(
            argv.iter().map(|argument| (*argument).to_owned()).collect(),
            Some(".".to_owned()),
            env_policy,
            stdin_text.map(str::to_owned),
            1024,
            1024,
        )
        .expect("test process intent is valid");
        ActionProposal::new(
            call,
            ToolActionKind::CommandExec,
            "process",
            argv.join(" "),
            "Classify process proposal.",
            ActionProposalEvidence::ProcessAction(intent),
        )
        .expect("test process proposal is valid")
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
    fn classifier_assigns_process_risk_from_process_proposal() {
        assert_eq!(
            classify_tool_action_risk(ToolActionKind::CommandExec, None),
            ActionRiskTier::ProcessHigh
        );

        let call = pending_tool_call();
        for argv in [["rustc", "--version"], ["rg", "--version"]] {
            let informational = process_proposal(&call, &argv);
            assert_eq!(
                classify_tool_action_risk(ToolActionKind::CommandExec, Some(&informational)),
                ActionRiskTier::ProcessLow
            );
        }

        let local_effect = process_proposal(&call, &["cargo", "test", "-p", "merry-runtime"]);
        assert_eq!(
            classify_tool_action_risk(ToolActionKind::CommandExec, Some(&local_effect)),
            ActionRiskTier::ProcessLocalWorkspaceEffect
        );

        let forbidden = process_proposal(&call, &["sh", "-c", "rm -rf target"]);
        assert_eq!(
            classify_tool_action_risk(ToolActionKind::CommandExec, Some(&forbidden)),
            ActionRiskTier::ProcessHigh
        );

        let unknown = process_proposal(&call, &["unknown-readonly-ish", "--version"]);
        assert_eq!(
            classify_tool_action_risk(ToolActionKind::CommandExec, Some(&unknown)),
            ActionRiskTier::ProcessLocalWorkspaceEffect
        );
    }

    #[test]
    fn process_admission_predicates_keep_low_and_local_workspace_lanes_distinct() {
        let call = pending_tool_call();
        let admission = AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace();
        let informational = process_proposal(&call, &["rustc", "--version"]);
        assert!(is_low_risk_process_action_proposal(
            ToolActionKind::CommandExec,
            &informational
        ));
        assert!(!is_local_workspace_effect_process_action_proposal(
            ToolActionKind::CommandExec,
            &informational,
            admission
        ));
        assert!(!is_read_only_shell_process_action_proposal(
            ToolActionKind::CommandExec,
            &informational
        ));

        let local_effect = process_proposal(&call, &["cargo", "test", "-p", "merry-runtime"]);
        assert!(!is_low_risk_process_action_proposal(
            ToolActionKind::CommandExec,
            &local_effect
        ));
        assert!(!is_read_only_shell_process_action_proposal(
            ToolActionKind::CommandExec,
            &local_effect
        ));
        assert!(is_local_workspace_effect_process_action_proposal(
            ToolActionKind::CommandExec,
            &local_effect,
            admission
        ));
        assert!(!is_local_workspace_effect_process_action_proposal(
            ToolActionKind::WorkspaceWrite,
            &local_effect,
            admission
        ));

        let local_effect_with_stdin = process_proposal_with_policy(
            &call,
            &["cargo", "test", "-p", "merry-runtime"],
            ProcessEnvPolicy::empty(),
            Some("stdin is not admitted"),
        );
        assert!(!is_local_workspace_effect_process_action_proposal(
            ToolActionKind::CommandExec,
            &local_effect_with_stdin,
            admission
        ));

        let local_effect_with_env = process_proposal_with_policy(
            &call,
            &["cargo", "test", "-p", "merry-runtime"],
            ProcessEnvPolicy::NonEmptyForTest,
            None,
        );
        assert!(!is_local_workspace_effect_process_action_proposal(
            ToolActionKind::CommandExec,
            &local_effect_with_env,
            admission
        ));

        let mismatched_admission =
            AcceptedLocalWorkspaceProcessAdmission::for_test_permission_profile_id(
                ProcessPermissionProfileId::READ_ONLY,
            );
        assert!(!is_local_workspace_effect_process_action_proposal(
            ToolActionKind::CommandExec,
            &local_effect,
            mismatched_admission
        ));

        let unknown_workspace_effect =
            process_proposal(&call, &["unknown-readonly-ish", "--version"]);
        assert!(!is_low_risk_process_action_proposal(
            ToolActionKind::CommandExec,
            &unknown_workspace_effect
        ));
        assert!(!is_local_workspace_effect_process_action_proposal(
            ToolActionKind::CommandExec,
            &unknown_workspace_effect,
            admission
        ));

        let shell_workspace_effect = process_proposal(
            &call,
            &[
                "bash",
                "-lc",
                "HOME=.merry/local/home cargo check --all-targets -p merry-runtime",
            ],
        );
        assert!(!is_read_only_shell_process_action_proposal(
            ToolActionKind::CommandExec,
            &shell_workspace_effect
        ));
        assert!(!is_local_workspace_effect_process_action_proposal(
            ToolActionKind::CommandExec,
            &shell_workspace_effect,
            admission
        ));

        let shell_read_only = process_proposal(&call, &["bash", "-lc", "rg ProcessRunner | wc -l"]);
        assert!(!is_low_risk_process_action_proposal(
            ToolActionKind::CommandExec,
            &shell_read_only
        ));
        assert!(!is_local_workspace_effect_process_action_proposal(
            ToolActionKind::CommandExec,
            &shell_read_only,
            admission
        ));
        assert!(is_read_only_shell_process_action_proposal(
            ToolActionKind::CommandExec,
            &shell_read_only
        ));
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
