use crate::{
    ActionExecutionEvidence, ActionProposal, RuntimeError,
    action_policy::{ActionPolicyDecision, is_low_risk_workspace_patch_proposal},
    tool::{ActionProposalEvidence, ToolExecutionContext},
};
use merry_core::{PendingToolCall, RuntimeEvent, RuntimeEventKind, SessionId};

use super::{
    DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED, TOOL_ACTION_POLICY_DENIED_MESSAGE,
    WORKSPACE_PATCH_TOOL_NAME, diagnostic_from_text,
};

pub(super) fn denied_tool_action_outcome(pending: &PendingToolCall) -> crate::ToolExecutionOutcome {
    let diagnostic = diagnostic_from_text(
        DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
        TOOL_ACTION_POLICY_DENIED_MESSAGE,
    );
    let payload = serde_json::json!({
        "ok": false,
        "tool": pending.name().as_str(),
        "error": {
            "code": DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
            "message": TOOL_ACTION_POLICY_DENIED_MESSAGE
        }
    });

    crate::ToolExecutionOutcome::failed_json(payload.to_string(), diagnostic)
}

pub(super) fn trace_denied_tool_execution(
    session_id: &str,
    pending: &PendingToolCall,
    events: &[RuntimeEvent],
) {
    tracing::info!(
        event = "runtime.tool.execute.finish",
        session_id,
        tool_call_id = pending.id().as_str(),
        tool_name = pending.name().as_str(),
        status = "denied",
        artifact_id = tool_resolution_artifact_id(events),
        diagnostic_code = DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
        "runtime tool execution denied"
    );
}

fn tool_resolution_artifact_id(events: &[RuntimeEvent]) -> String {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => {
                Some(result.artifact().id().as_str().to_owned())
            }
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn validate_action_proposal(
    proposal: &ActionProposal,
    pending: &PendingToolCall,
    action_kind: crate::ToolActionKind,
    session_id: &SessionId,
) -> Result<(), RuntimeError> {
    proposal
        .validate_for_call(pending, action_kind)
        .map_err(|reason| RuntimeError::ToolExecutionFailed {
            session_id: session_id.clone(),
            call_id: pending.id().clone(),
            message: reason.to_owned(),
        })
}

pub(super) fn context_with_approved_proposal(
    context: ToolExecutionContext,
    proposal: Option<&ActionProposal>,
) -> ToolExecutionContext {
    match proposal.map(ActionProposal::evidence) {
        Some(ActionProposalEvidence::WorkspacePatch(patch)) => {
            context.with_approved_workspace_patch(patch.clone())
        }
        Some(ActionProposalEvidence::ProcessAction(_)) | None => context,
    }
}

pub(super) fn action_execution_evidence_matches_proposal(
    proposal: &ActionProposal,
    execution_evidence: &ActionExecutionEvidence,
) -> bool {
    match (proposal.evidence(), execution_evidence) {
        (
            ActionProposalEvidence::WorkspacePatch(proposed),
            ActionExecutionEvidence::WorkspacePatch(executed),
        ) => proposed.changes() == executed.changes(),
        (
            ActionProposalEvidence::ProcessAction(proposed),
            ActionExecutionEvidence::ProcessAction(executed),
        ) => executed.matches_intent(proposed),
        _ => false,
    }
}

pub(super) fn admit_action_to_generic_executor(
    pending: &PendingToolCall,
    action_kind: crate::ToolActionKind,
    decision: &ActionPolicyDecision,
    proposal: Option<&ActionProposal>,
    session_id: &SessionId,
) -> Result<(), RuntimeError> {
    if !action_kind.is_mutating() {
        return Ok(());
    }

    let low_risk_workspace_patch_admitted = decision.is_allowed()
        && decision.action_kind() == crate::ToolActionKind::WorkspaceWrite
        && decision.risk_tier() == crate::action_policy::ActionRiskTier::EditLow
        && pending.name().as_str() == WORKSPACE_PATCH_TOOL_NAME
        && action_kind == crate::ToolActionKind::WorkspaceWrite
        && proposal
            .is_some_and(|proposal| is_low_risk_workspace_patch_proposal(action_kind, proposal));

    if !low_risk_workspace_patch_admitted {
        return Err(RuntimeError::MutatingActionCommitLifecycleRequired {
            session_id: session_id.clone(),
            call_id: pending.id().clone(),
            action_kind,
        });
    }

    Ok(())
}
