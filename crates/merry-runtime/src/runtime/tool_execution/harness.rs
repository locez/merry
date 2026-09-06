//! Side-effect-free Plan harness admission before tool execution.

use crate::{ActionProposal, tool::ActionProposalEvidence};
use merry_core::{ErrorInfo, PendingToolCall, PlanHarnessSnapshot};
use std::path::Path;

pub(super) fn plan_harness_violation(
    harness: Option<&PlanHarnessSnapshot>,
    pending: &PendingToolCall,
    action_kind: crate::ToolActionKind,
    proposal: Option<&ActionProposal>,
) -> Option<ErrorInfo> {
    let harness = harness?;
    if !harness
        .allowed_tools
        .iter()
        .any(|allowed| allowed == pending.name())
    {
        return Some(plan_harness_diagnostic(
            "plan_harness_tool_denied",
            format!(
                "tool {} is outside the active plan node harness",
                pending.name()
            ),
        ));
    }

    let effective_path = pending
        .arguments()
        .as_object()
        .get("path")
        .and_then(serde_json::Value::as_str);
    if let Some(path) = effective_path {
        let scopes = if action_kind == crate::ToolActionKind::WorkspaceWrite {
            &harness.write_scope
        } else {
            &harness.read_scope
        };
        if !plan_harness_allows_path(harness, scopes, path) {
            return Some(plan_harness_scope_diagnostic(path));
        }
    }

    match proposal.map(ActionProposal::evidence) {
        Some(ActionProposalEvidence::WorkspacePatch(patch)) => {
            for change in patch.changes() {
                if !plan_harness_allows_path(harness, &harness.write_scope, change.relative_path())
                {
                    return Some(plan_harness_scope_diagnostic(change.relative_path()));
                }
            }
        }
        Some(ActionProposalEvidence::ProcessAction(intent)) => {
            let path = intent.cwd().unwrap_or(".");
            let scopes = match crate::process::classify_process_intent(intent) {
                crate::process::ProcessIntentClass::Informational => &harness.read_scope,
                crate::process::ProcessIntentClass::LocalWorkspaceEffect
                | crate::process::ProcessIntentClass::Unknown
                | crate::process::ProcessIntentClass::Forbidden => &harness.write_scope,
            };
            if !plan_harness_allows_path(harness, scopes, path) {
                return Some(plan_harness_scope_diagnostic(path));
            }
        }
        None => {}
    }
    None
}

pub(super) fn plan_harness_allows_path(
    harness: &PlanHarnessSnapshot,
    scopes: &[String],
    path: &str,
) -> bool {
    let path = Path::new(path);
    let in_scope = scopes
        .iter()
        .any(|scope| crate::workspace_scope::workspace_scope_contains(Path::new(scope), path));
    let overlaps_forbidden = harness.forbidden_paths.iter().any(|forbidden| {
        let forbidden = Path::new(forbidden);
        crate::workspace_scope::workspace_scope_contains(forbidden, path)
            || crate::workspace_scope::workspace_scope_contains(path, forbidden)
    });
    in_scope && !overlaps_forbidden
}

pub(super) fn plan_harness_scope_diagnostic(path: &str) -> ErrorInfo {
    plan_harness_diagnostic(
        "plan_harness_scope_denied",
        format!("workspace path {path} is outside the active plan node harness"),
    )
}

pub(super) fn plan_harness_diagnostic(code: &'static str, message: String) -> ErrorInfo {
    crate::runtime::diagnostic_from_text(code, message)
}
