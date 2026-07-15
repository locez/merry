use crate::{
    ArtifactContent, PlanControllerError, PlanError, RuntimeError,
    plan::{
        ReadPlanInput, UpdatePlanInput,
        tools::{READ_PLAN_TOOL_NAME, UPDATE_PLAN_TOOL_NAME},
    },
    tool::ToolExecutionContext,
};
use merry_core::{
    ErrorInfo, PendingToolCall, PlanActivationSource, PlanApprovalRequirementSnapshot,
    PlanCapabilityEnvelopeSnapshot, PlanExecutionSummary, PlanId, PlanLinkSnapshot, PlanNodeId,
    PlanNodeResult, PlanNodeStatus, PlanPhase, PlanRevisionSummary, PlanSnapshot,
    RuntimeJournalEvent, ToolCallResultStatus,
};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;

use super::{RuntimeInner, persist_resume_safe_savepoint_if_configured};

const PLAN_READ_MAX_DEPTH: u8 = 16;

pub(super) async fn execute_plan_tool_call(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    _context: ToolExecutionContext,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    match pending.name().as_str() {
        READ_PLAN_TOOL_NAME => {
            let input = input_from_call::<ReadPlanInput>(inner, pending)?;
            match read_plan(inner, input).await {
                Ok(output) => submit_succeeded(inner, pending, output, Vec::new()).await,
                Err(error) => submit_rejection(inner, pending, error).await,
            }
        }
        UPDATE_PLAN_TOOL_NAME => {
            let input = input_from_call::<UpdatePlanInput>(inner, pending)?;
            match inner
                .plan_controller
                .update_from_tool(input, pending.id().clone())
                .await
            {
                Ok(events) => Ok(events),
                Err(error) => submit_controller_error(inner, pending, error).await,
            }
        }
        _ => Err(RuntimeError::ToolExecutionFailed {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
            message: format!(
                "plan tool {} is not implemented for this runtime role",
                pending.name()
            ),
        }),
    }
}

#[derive(Serialize)]
struct ReadPlanOutput {
    snapshot: ReadPlanSnapshot,
    selected_node_id: Option<PlanNodeId>,
    next_cursor: Option<String>,
    guidance: ReadPlanGuidance,
}

#[derive(Serialize)]
struct ReadPlanGuidance {
    do_not_repeat_until_state_change: bool,
    instruction: &'static str,
}

#[derive(Serialize)]
struct ReadPlanSnapshot {
    plan_id: PlanId,
    revision: u64,
    phase: PlanPhase,
    activation_source: PlanActivationSource,
    root_node_id: Option<PlanNodeId>,
    coordinator_node_id: Option<PlanNodeId>,
    execution_contract_fingerprint: Option<String>,
    execution_authorization_refs: Vec<String>,
    authorized_capability_envelope: Option<PlanCapabilityEnvelopeSnapshot>,
    approval_requirements: Vec<PlanApprovalRequirementSnapshot>,
    nodes: Vec<ReadPlanNode>,
    max_concurrency_hint: Option<usize>,
    revision_summaries: Vec<PlanRevisionSummary>,
}

#[derive(Serialize)]
struct ReadPlanNode {
    id: PlanNodeId,
    client_key: Option<String>,
    parent_id: Option<PlanNodeId>,
    sibling_order: u16,
    objective: String,
    acceptance: Vec<String>,
    status: PlanNodeStatus,
    depends_on: Vec<PlanNodeId>,
    result: Option<PlanNodeResult>,
    created_revision: u64,
    updated_revision: u64,
    execution_summary: PlanExecutionSummary,
    links: Vec<PlanLinkSnapshot>,
}

impl From<&PlanSnapshot> for ReadPlanSnapshot {
    fn from(snapshot: &PlanSnapshot) -> Self {
        Self {
            plan_id: snapshot.plan_id.clone(),
            revision: snapshot.revision,
            phase: snapshot.phase,
            activation_source: snapshot.activation_source.clone(),
            root_node_id: snapshot.root_node_id.clone(),
            coordinator_node_id: snapshot.coordinator_node_id.clone(),
            execution_contract_fingerprint: snapshot.execution_contract_fingerprint.clone(),
            execution_authorization_refs: snapshot.execution_authorization_refs.clone(),
            authorized_capability_envelope: snapshot.authorized_capability_envelope.clone(),
            approval_requirements: snapshot.approval_requirements.clone(),
            nodes: snapshot
                .nodes
                .iter()
                .map(|node| ReadPlanNode {
                    id: node.id.clone(),
                    client_key: node.client_key.clone(),
                    parent_id: node.parent_id.clone(),
                    sibling_order: node.sibling_order,
                    objective: node.objective.clone(),
                    acceptance: node.acceptance.clone(),
                    status: node.status,
                    depends_on: node.depends_on.clone(),
                    result: node.result.clone(),
                    created_revision: node.created_revision,
                    updated_revision: node.updated_revision,
                    execution_summary: node.execution_summary.clone(),
                    links: node.links.clone(),
                })
                .collect(),
            max_concurrency_hint: snapshot.max_concurrency_hint,
            revision_summaries: snapshot.revision_summaries.clone(),
        }
    }
}

async fn read_plan(
    inner: &RuntimeInner,
    input: ReadPlanInput,
) -> Result<ReadPlanOutput, PlanToolRejection> {
    let mut snapshot = {
        let session = inner.session.lock().await;
        match input.plan_id.as_ref() {
            Some(plan_id) => session
                .active_plan()
                .map(|plan| plan.snapshot())
                .filter(|snapshot| &snapshot.plan_id == plan_id)
                .or_else(|| {
                    session
                        .terminal_plans()
                        .iter()
                        .find(|snapshot| &snapshot.plan_id == plan_id)
                })
                .cloned()
                .ok_or_else(|| {
                    PlanToolRejection::new(
                        "plan_not_found",
                        format!("plan {plan_id} was not found"),
                    )
                })?,
            None => session
                .active_plan()
                .map(|plan| plan.snapshot().clone())
                .ok_or_else(no_active_plan_rejection)?,
        }
    };

    let max_depth = input.max_depth.unwrap_or(PLAN_READ_MAX_DEPTH);
    if max_depth > PLAN_READ_MAX_DEPTH {
        return Err(PlanToolRejection::new(
            "plan_read_depth_exceeded",
            format!("max_depth must be at most {PLAN_READ_MAX_DEPTH}"),
        ));
    }

    let selected_node_id = input.node_id.as_ref().or(snapshot.root_node_id.as_ref());
    let selected_ids = selected_node_id
        .map(|node_id| subtree_ids(&snapshot, node_id, max_depth))
        .transpose()?;
    if let Some(selected_ids) = selected_ids.as_ref() {
        snapshot
            .nodes
            .retain(|node| selected_ids.contains(&node.id));
    }

    // Attempts, leases, progress, and directives belong to the removed model
    // reporting protocol. Keep them in durable history for migration/debugging,
    // but never put them back into the provider-visible Plan projection.
    snapshot.attempts.clear();
    snapshot.leases.clear();
    snapshot.attempt_progress.clear();
    snapshot.directives.clear();

    Ok(ReadPlanOutput {
        snapshot: ReadPlanSnapshot::from(&snapshot),
        selected_node_id: input.node_id,
        next_cursor: None,
        guidance: ReadPlanGuidance {
            do_not_repeat_until_state_change: true,
            instruction: "Use this exact snapshot for the next decision. Do not call read_plan again unless a runtime event changed the Plan; continue ordinary work or use update_plan for an actual authored revision.",
        },
    })
}

fn subtree_ids(
    snapshot: &PlanSnapshot,
    selected: &PlanNodeId,
    max_depth: u8,
) -> Result<BTreeSet<PlanNodeId>, PlanToolRejection> {
    if !snapshot.nodes.iter().any(|node| &node.id == selected) {
        return Err(PlanToolRejection::new(
            "plan_node_not_found",
            format!("node {selected} was not found in plan {}", snapshot.plan_id),
        ));
    }
    let mut selected_ids = BTreeSet::from([selected.clone()]);
    let mut frontier = vec![selected.clone()];
    for _ in 0..max_depth {
        let mut next = Vec::new();
        for node in &snapshot.nodes {
            if node
                .parent_id
                .as_ref()
                .is_some_and(|parent| frontier.contains(parent))
                && selected_ids.insert(node.id.clone())
            {
                next.push(node.id.clone());
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(selected_ids)
}

fn input_from_call<T>(inner: &RuntimeInner, pending: &PendingToolCall) -> Result<T, RuntimeError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::Object(
        pending.arguments().as_object().clone(),
    ))
    .map_err(|error| RuntimeError::ToolExecutionFailed {
        session_id: inner.session_id.clone(),
        call_id: pending.id().clone(),
        message: format!("validated plan tool input could not be decoded: {error}"),
    })
}

async fn submit_succeeded<T>(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    output: T,
    mut plan_events: Vec<RuntimeJournalEvent>,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError>
where
    T: Serialize,
{
    let content =
        serde_json::to_string(&output).map_err(|error| RuntimeError::ToolExecutionFailed {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
            message: format!("failed to serialize {} output: {error}", pending.name()),
        })?;
    let events = {
        let mut session = inner.session.lock().await;
        session.submit_tool_execution_outcome(
            pending.id(),
            ToolCallResultStatus::Succeeded,
            ArtifactContent::json(content),
            None,
            None,
        )?
    };
    plan_events.extend(events);
    persist_resume_safe_savepoint_if_configured(inner).await;
    Ok(plan_events)
}

async fn submit_controller_error(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    error: PlanControllerError,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    match error {
        PlanControllerError::SessionStore { .. }
        | PlanControllerError::CommandChannelClosed
        | PlanControllerError::StaleTransaction
        | PlanControllerError::Runtime { .. } => Err(RuntimeError::ToolExecutionFailed {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
            message: error.to_string(),
        }),
        PlanControllerError::Plan { source } => {
            submit_rejection(inner, pending, plan_error_rejection(&source)).await
        }
        PlanControllerError::NoActivePlan => {
            submit_rejection(inner, pending, no_active_plan_rejection()).await
        }
    }
}

async fn submit_rejection(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    rejection: PlanToolRejection,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let PlanToolRejection {
        code,
        message,
        recovery,
    } = rejection;
    let diagnostic = ErrorInfo::new(code, &message).map_err(RuntimeError::from)?;
    let content = serde_json::json!({
        "ok": false,
        "tool": pending.name().as_str(),
        "error": {
            "code": code,
            "message": message,
        },
        "recovery": recovery,
    });
    let events = {
        let mut session = inner.session.lock().await;
        session.submit_tool_execution_outcome(
            pending.id(),
            ToolCallResultStatus::Failed,
            ArtifactContent::json(content.to_string()),
            Some(diagnostic),
            None,
        )?
    };
    persist_resume_safe_savepoint_if_configured(inner).await;
    Ok(events)
}

struct PlanToolRejection {
    code: &'static str,
    message: String,
    recovery: serde_json::Value,
}

impl PlanToolRejection {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            recovery: read_plan_recovery(),
        }
    }

    fn with_recovery(mut self, recovery: serde_json::Value) -> Self {
        self.recovery = recovery;
        self
    }
}

fn no_active_plan_rejection() -> PlanToolRejection {
    PlanToolRejection::new("no_active_plan", "no active plan exists").with_recovery(
        serde_json::json!({
            "next_tool": "update_plan",
            "instruction": "Do not call read_plan again. If a durable plan is useful, use update_plan with expected_plan_revision 0 to define the first plan tree; the first valid update creates the active Plan. Otherwise continue with ordinary registered tools.",
            "example": {
                "reason": "Coordinate the requested multi-step work",
                "execution_intent": "continue_planning",
                "change": {
                    "type": "define_plan",
                    "expected_plan_revision": 0,
                    "root": {
                        "client_key": "root",
                        "objective": "Complete the requested work",
                        "acceptance": ["Focused checks pass"],
                        "depends_on": [],
                        "children": []
                    }
                }
            }
        }),
    )
}

fn plan_error_rejection(error: &PlanError) -> PlanToolRejection {
    let recovery = match error {
        PlanError::WrongPhase {
            actual: PlanPhase::AwaitingApproval,
            ..
        } => serde_json::json!({
            "actor": "user",
            "next_action": "approve_or_request_revision_in_plan_ui",
            "instruction": "Explain the pending approval requirement and let the user approve or revise it. Plan approval does not disable ordinary tools; use update_plan only for an explicit plan revision and keep runtime permission checks for any action."
        }),
        PlanError::WrongPhase {
            actual: PlanPhase::Executing,
            ..
        } => serde_json::json!({
            "next_action": "continue_current_plan",
            "instruction": "The Plan is already executing. Do not use update_plan with use_current_plan again. Continue ordinary work or inspect the current snapshot once with read_plan; revise only a mutable future subtree when the authored objective actually changed."
        }),
        PlanError::InvalidNewNodeIdentity | PlanError::InvalidExistingNodeIdentity => {
            serde_json::json!({
                "next_tool": "update_plan",
                "instruction": "For every new node omit id and provide one unique client_key. For an existing mutable node provide its runtime id and omit client_key.",
                "new_node_identity_example": { "client_key": "implementation" },
                "existing_node_identity_example": { "id": "plan-node-1" }
            })
        }
        PlanError::InvalidScopePath { .. } => serde_json::json!({
            "next_tool": "update_plan",
            "instruction": "Use '.' for the workspace root or a concrete normalized workspace-relative path. Do not use '..', absolute paths, embedded '.' segments, empty segments, or backslashes.",
            "valid_examples": [".", "crates/merry-runtime", "examples/config.toml"]
        }),
        PlanError::CapabilityEnvelopeExceeded { .. } => serde_json::json!({
            "actor": "coordinator_or_user",
            "next_action": "request_runtime_capability_or_plan_approval",
            "instruction": "The requested runtime capability is outside the current Plan authorization. Ask for the required permission or revise the authored plan; node harness fields are runtime-owned and cannot be widened by the model."
        }),
        PlanError::StalePlanRevision { .. } | PlanError::StaleNodeRevision { .. } => {
            read_plan_recovery()
        }
        PlanError::SubagentScopeViolation { .. } => serde_json::json!({
            "next_action": "use_bound_subagent_scope",
            "instruction": "The child scope cannot perform this operation outside its active binding. Use the binding's subtree for scoped reads and updates, and do not target nodes or links outside that subtree.",
        }),
        _ => read_plan_recovery(),
    };
    PlanToolRejection::new(plan_error_code(error), error.to_string()).with_recovery(recovery)
}

fn read_plan_recovery() -> serde_json::Value {
    serde_json::json!({
        "next_tool": "read_plan",
        "instruction": "Read the latest exact plan state before retrying a revision-sensitive operation."
    })
}

fn plan_error_code(error: &PlanError) -> &'static str {
    match error {
        PlanError::InvalidText { .. } => "plan_invalid_text",
        PlanError::WrongPhase { .. } => "plan_wrong_phase",
        PlanError::StalePlanIdentity { .. } => "plan_stale_identity",
        PlanError::StalePlanRevision { .. } => "plan_stale_revision",
        PlanError::StaleNodeRevision { .. } => "plan_stale_node_revision",
        PlanError::RootMissing | PlanError::RootHasParent => "plan_invalid_root",
        PlanError::NodeMissingParent { .. }
        | PlanError::UnknownParent { .. }
        | PlanError::UnreachableNode { .. }
        | PlanError::ParentCycle => "plan_invalid_parent_topology",
        PlanError::DependencyCycle
        | PlanError::SelfDependency { .. }
        | PlanError::DependsOnDescendant { .. }
        | PlanError::UnknownDependency { .. }
        | PlanError::IncomingDependencyWouldDangle { .. } => "plan_invalid_dependency_topology",
        PlanError::UnknownNode { .. } => "plan_node_not_found",
        PlanError::UnknownClientKey { .. }
        | PlanError::DuplicateClientKey { .. }
        | PlanError::InvalidNewNodeIdentity
        | PlanError::InvalidExistingNodeIdentity => "plan_invalid_node_identity",
        PlanError::DuplicateNodeId { .. } => "plan_duplicate_node_id",
        PlanError::TooManyNodes { .. }
        | PlanError::TooManyChildren { .. }
        | PlanError::PlanTooDeep { .. }
        | PlanError::TooManyDependencies { .. }
        | PlanError::TooManyAcceptanceItems { .. } => "plan_limit_exceeded",
        PlanError::DuplicateSiblingOrder { .. } => "plan_duplicate_sibling_order",
        PlanError::InvalidScopePath { .. } => "plan_invalid_scope_path",
        PlanError::CapabilityEnvelopeExceeded { .. } => "plan_capability_envelope_exceeded",
        PlanError::NodeNotMutable { .. } => "plan_node_not_mutable",
        PlanError::ReplacementRootIdentity { .. } => "plan_invalid_replacement_root",
        PlanError::InvalidConcurrencyHint { .. } => "plan_invalid_concurrency_hint",
        PlanError::InvalidPersistedCounters => "plan_persisted_state_invalid",
        PlanError::EmptyPlan => "plan_empty",
        PlanError::UnresolvedApprovalRequirement { .. } => "plan_approval_requirement_unresolved",
        PlanError::ActiveAttemptsPreventControl { .. } => "plan_active_attempts_prevent_control",
        PlanError::NodeNotReady { .. } => "plan_node_not_ready",
        PlanError::LiveLeaseExists { .. } => "plan_live_lease_exists",
        PlanError::InterruptedRetryUnavailable { .. } => "plan_interrupted_retry_unavailable",
        PlanError::UnknownLease { .. } => "plan_lease_not_found",
        PlanError::LeaseNotLive { .. } => "plan_lease_not_live",
        PlanError::UnknownAttempt { .. } => "plan_attempt_not_found",
        PlanError::NoActiveAttemptForExecutor { .. } => "plan_active_attempt_not_found",
        PlanError::MultipleActiveAttemptsForExecutor { .. } => "plan_active_attempt_ambiguous",
        PlanError::AttemptOwnershipMismatch { .. } => "plan_attempt_owner_mismatch",
        PlanError::AttemptAlreadyResolved { .. } => "plan_attempt_already_resolved",
        PlanError::AttemptNodeRevisionMismatch { .. } => "plan_attempt_node_revision_stale",
        PlanError::InvalidAttemptOutcome { .. } => "plan_attempt_outcome_invalid",
        PlanError::EmptyDecomposition | PlanError::NestedDecomposition => {
            "plan_decomposition_invalid"
        }
        PlanError::UnknownDirective { .. } => "plan_directive_not_found",
        PlanError::InvalidDirectiveTransition { .. } => "plan_directive_transition_invalid",
        PlanError::StaleDirectiveTarget => "plan_directive_target_stale",
        PlanError::MissingArtifactRef { .. } => "plan_artifact_ref_missing",
        PlanError::InvalidEvidenceRef { .. } => "plan_evidence_ref_invalid",
        PlanError::ArtifactPromotionConflict { .. } => "plan_artifact_promotion_conflict",
        PlanError::InvalidAuthoredNodeStatus { .. } => "plan_invalid_authored_status",
        PlanError::SubagentScopeViolation { .. } => "plan_subagent_scope_violation",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executing_plan_recovery_does_not_send_use_current_plan_back_to_model() {
        let rejection = plan_error_rejection(&PlanError::WrongPhase {
            actual: PlanPhase::Executing,
            operation: "use current plan",
        });
        assert_eq!(rejection.code, "plan_wrong_phase");
        assert_eq!(rejection.recovery["next_action"], "continue_current_plan");
        assert!(
            rejection.recovery["instruction"]
                .as_str()
                .expect("recovery instruction should be text")
                .contains("Do not use update_plan with use_current_plan")
        );
    }

    #[test]
    fn subagent_scope_violation_recovery_requires_the_bound_subtree() {
        let rejection = plan_error_rejection(&PlanError::SubagentScopeViolation {
            reason: "scope root is not owned by the linked binding",
        });
        assert_eq!(rejection.code, "plan_subagent_scope_violation");
        assert_eq!(
            rejection.recovery["next_action"],
            "use_bound_subagent_scope"
        );
        let instruction = rejection.recovery["instruction"]
            .as_str()
            .expect("recovery instruction should be text");
        assert!(instruction.contains("active binding"));
        assert!(instruction.contains("binding's subtree"));
    }
}
