use crate::{
    ArtifactContent, PlanControllerError, PlanError, RuntimeError,
    plan::{
        PLAN_READ_MAX_DEPTH, ReadPlanInput, SubagentPlanUpdateInput, UpdatePlanInput,
        tools::{READ_PLAN_TOOL_NAME, UPDATE_PLAN_TOOL_NAME},
    },
    subagent::PlanSubagentScope,
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

use super::RuntimeInner;

pub(super) async fn execute_plan_tool_call(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    _context: ToolExecutionContext,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    if let Some(scope) = inner.plan_subagent_scope.as_ref() {
        return execute_scoped_plan_tool_call(inner, pending, scope).await;
    }
    if !inner.coordinator_plan_tools {
        return Err(plan_tool_role_error(inner, pending));
    }
    match pending.name().as_str() {
        READ_PLAN_TOOL_NAME => {
            let input = match input_from_call::<ReadPlanInput>(pending) {
                Ok(input) => input,
                Err(error) => return submit_input_decode_error(inner, pending, error).await,
            };
            match read_plan(inner, input).await {
                Ok(output) => submit_succeeded(inner, pending, output, Vec::new()).await,
                Err(error) => submit_rejection(inner, pending, error).await,
            }
        }
        UPDATE_PLAN_TOOL_NAME => {
            let input = match input_from_call::<UpdatePlanInput>(pending) {
                Ok(input) => input,
                Err(error) => return submit_input_decode_error(inner, pending, error).await,
            };
            match inner
                .plan_controller
                .update_from_tool(input, pending.id().clone(), !inner.tool_batch_active())
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

async fn execute_scoped_plan_tool_call(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    scope: &PlanSubagentScope,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    match pending.name().as_str() {
        READ_PLAN_TOOL_NAME => {
            let input = match input_from_call::<ReadPlanInput>(pending) {
                Ok(input) => input,
                Err(error) => return submit_input_decode_error(inner, pending, error).await,
            };
            let snapshot = match scope.read().await {
                Ok(snapshot) => snapshot,
                Err(error) => return submit_controller_error(inner, pending, error).await,
            };
            if input
                .plan_id
                .as_ref()
                .is_some_and(|plan_id| plan_id != &snapshot.plan_id)
            {
                return submit_rejection(
                    inner,
                    pending,
                    plan_error_rejection(&PlanError::SubagentScopeViolation {
                        reason: "scoped read cannot select another plan",
                    }),
                )
                .await;
            }
            match read_plan_snapshot(snapshot, input) {
                Ok(output) => submit_succeeded(inner, pending, output, Vec::new()).await,
                Err(error) => submit_rejection(inner, pending, error).await,
            }
        }
        UPDATE_PLAN_TOOL_NAME => {
            let input = match input_from_call::<SubagentPlanUpdateInput>(pending) {
                Ok(input) => input,
                Err(error) => return submit_input_decode_error(inner, pending, error).await,
            };
            match scope.update_plan(input).await {
                Ok(output) => submit_succeeded(inner, pending, output, Vec::new()).await,
                Err(error) => submit_controller_error(inner, pending, error).await,
            }
        }
        _ => Err(plan_tool_role_error(inner, pending)),
    }
}

fn plan_tool_role_error(inner: &RuntimeInner, pending: &PendingToolCall) -> RuntimeError {
    RuntimeError::ToolExecutionFailed {
        session_id: inner.session_id.clone(),
        call_id: pending.id().clone(),
        message: format!(
            "plan tool {} is not implemented for this runtime role",
            pending.name()
        ),
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
    let snapshot = {
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
    read_plan_snapshot(snapshot, input)
}

fn read_plan_snapshot(
    mut snapshot: PlanSnapshot,
    input: ReadPlanInput,
) -> Result<ReadPlanOutput, PlanToolRejection> {
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

struct PlanInputDecodeError {
    path: String,
    message: String,
}

fn input_from_call<T>(pending: &PendingToolCall) -> Result<T, PlanInputDecodeError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::Object(
        pending.arguments().as_object().clone(),
    ))
    .map_err(|error| PlanInputDecodeError {
        path: input_decode_path(pending),
        message: error.to_string(),
    })
}

async fn submit_input_decode_error(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    error: PlanInputDecodeError,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    submit_rejection(
        inner,
        pending,
        input_decode_rejection(inner, pending, error),
    )
    .await
}

fn input_decode_rejection(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    error: PlanInputDecodeError,
) -> PlanToolRejection {
    let PlanInputDecodeError {
        path,
        message: detail,
    } = error;
    let mut message = format!(
        "could not decode {} input at `{path}`: {detail}",
        pending.name(),
    );
    if let Some(field) = misplaced_update_plan_field(pending) {
        if inner.plan_subagent_scope.is_some() {
            message.push_str(&format!(
                ". `{field}` is not valid inside a scoped update_plan request; keep only `reason` and `change` at the top level"
            ));
        } else {
            message.push_str(&format!(
                ". `{field}` is an outer update_plan field and must be at the top level alongside `change`, not inside `change`"
            ));
        }
    }
    let recovery = if pending.name().as_str() == UPDATE_PLAN_TOOL_NAME {
        let scoped = inner.plan_subagent_scope.is_some();
        if path == "change.type" {
            if scoped {
                message.push_str(
                    ". `change.type` is required inside the change object; valid values are define_children and replace_subtree",
                );
            } else {
                message.push_str(
                    ". `change.type` is required inside the change object; valid values are define_plan, replace_subtree, and use_current_plan",
                );
            }
        }
        if scoped {
            serde_json::json!({
                "next_tool": UPDATE_PLAN_TOOL_NAME,
                "instruction": "Retry with valid JSON. For a scoped update, put reason and change at the top level. The change field must be an object whose nested type is one of define_children or replace_subtree; do not put type or coordinator metadata on the outer update object.",
                "outer_fields": ["reason", "change"],
                "nested_change_fields": ["type", "expected_plan_revision", "children", "target_node_id", "expected_node_revision", "subtree"],
                "required_field": "change.type",
                "valid_types": ["define_children", "replace_subtree"],
            })
        } else {
            serde_json::json!({
                "next_tool": UPDATE_PLAN_TOOL_NAME,
                "instruction": "Retry with valid JSON. Put reason, execution_intent, coordinator_node_id, and max_concurrency_hint at the top level alongside change. Put only the nested change.type discriminator and its variant-specific fields inside change; do not move outer metadata into change or put type on the outer update object.",
                "outer_fields": ["reason", "execution_intent", "coordinator_node_id", "max_concurrency_hint", "change"],
                "nested_change_fields": ["type", "expected_plan_revision", "root", "target_node_id", "expected_node_revision", "subtree"],
                "required_field": "change.type",
                "example": crate::plan::update_plan_define_example(),
            })
        }
    } else {
        serde_json::json!({
            "next_tool": pending.name().as_str(),
            "instruction": "Retry this tool with corrected JSON arguments using the exact field names and types from its schema.",
        })
    };

    PlanToolRejection::new("plan_input_invalid", message).with_recovery(recovery)
}

fn input_decode_path(pending: &PendingToolCall) -> String {
    if pending.name().as_str() == UPDATE_PLAN_TOOL_NAME
        && pending
            .arguments()
            .as_object()
            .get("change")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|change| !change.contains_key("type"))
    {
        return "change.type".to_owned();
    }
    if let Some(field) = misplaced_update_plan_field(pending) {
        return format!("change.{field}");
    }

    "input".to_owned()
}

fn misplaced_update_plan_field(pending: &PendingToolCall) -> Option<&'static str> {
    if pending.name().as_str() != UPDATE_PLAN_TOOL_NAME {
        return None;
    }
    let change = pending
        .arguments()
        .as_object()
        .get("change")
        .and_then(serde_json::Value::as_object)?;
    [
        "reason",
        "execution_intent",
        "coordinator_node_id",
        "max_concurrency_hint",
    ]
    .into_iter()
    .find(|field| change.contains_key(*field))
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
            "instruction": "The Plan is already executing. Do not use update_plan with use_current_plan again. Continue ordinary work or inspect the current snapshot once with read_plan; revise only a mutable future subtree when the authored objective actually changed. If the user explicitly asked to start over, call update_plan with define_plan, a fresh root, and direct children; do not provide target_node_id."
        }),
        PlanError::WrongPhase {
            actual: PlanPhase::Blocked,
            ..
        } => serde_json::json!({
            "next_tool": "read_plan",
            "next_action": "inspect_and_resume_blocked_execution",
            "instruction": "The Plan is blocked because linked work stopped before completion. Read the current snapshot first. If the linked child status is blocked with subagent_max_model_turns_reached, do not use update_plan.replace_subtree: call spawn_subagents with the same authored plan_client_key and a larger budget within the configured maximum; use a larger value for complex tasks. The runtime will supersede the old blocked link and reopen the Plan while the replacement continues from the shared workspace."
        }),
        PlanError::WrongPhase {
            actual: PlanPhase::Completed | PlanPhase::Cancelled,
            ..
        } => serde_json::json!({
            "next_tool": "update_plan",
            "instruction": "The previous Plan is terminal. If the user wants another run, call update_plan with define_plan, expected_plan_revision 0, a fresh root, and direct children. Do not provide target_node_id or reuse old node ids."
        }),
        PlanError::NodeNotMutable {
            status: PlanNodeStatus::Verifying | PlanNodeStatus::Completed,
            ..
        } => serde_json::json!({
            "next_tool": "update_plan",
            "instruction": "This runtime-owned node is already verifying or complete. Do not retry update_plan with its target_node_id. If the user wants a fresh run, call update_plan with define_plan, a fresh root, and direct children; if a check is needed, author it as a verification child."
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
        PlanError::ActiveSubagentOwnsSubtree { .. } => serde_json::json!({
            "actor": "coordinator",
            "next_action": "wait_or_cancel_and_create_new_assignment",
            "instruction": "This node or subtree is owned by an active linked child. Do not retry the replacement. Continue unrelated work, wait for the child to reach a terminal result, or cancel the child and create a new assignment; do not rewrite this node after cancellation because terminal link history is preserved.",
        }),
        PlanError::ActiveAttemptsPreventControl { .. } => serde_json::json!({
            "next_action": "wait_for_active_work",
            "instruction": "The runtime will not discard active attempts, leases, or linked work. Wait for the active work to reach a terminal result, then call update_plan with define_plan and a fresh root if the user still wants a fresh run.",
        }),
        PlanError::NestedPlanInput => serde_json::json!({
            "next_tool": "update_plan",
            "instruction": "Keep coordinator-authored plan input shallow: provide the root and direct children only. Do not nest children under a child node; a linked child owns deeper decomposition in its scoped Plan.",
        }),
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
        | PlanError::TooManyAcceptanceItems { .. }
        | PlanError::TooManyPayloadItems { .. }
        | PlanError::TooManyTransientAttempts { .. }
        | PlanError::TooManyActiveDirectives { .. }
        | PlanError::SnapshotTooLarge { .. } => "plan_limit_exceeded",
        PlanError::DuplicateSiblingOrder { .. } => "plan_duplicate_sibling_order",
        PlanError::InvalidScopePath { .. } => "plan_invalid_scope_path",
        PlanError::CapabilityEnvelopeExceeded { .. } => "plan_capability_envelope_exceeded",
        PlanError::NodeNotMutable { .. } => "plan_node_not_mutable",
        PlanError::ActiveSubagentOwnsSubtree { .. } => "plan_active_subagent_owns_subtree",
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
        PlanError::ActiveAttemptExistsForExecutor { .. } => "plan_active_attempt_exists",
        PlanError::MultipleActiveAttemptsForExecutor { .. } => "plan_active_attempt_ambiguous",
        PlanError::AttemptOwnershipMismatch { .. } => "plan_attempt_owner_mismatch",
        PlanError::AttemptAlreadyResolved { .. } => "plan_attempt_already_resolved",
        PlanError::AttemptNodeRevisionMismatch { .. } => "plan_attempt_node_revision_stale",
        PlanError::InvalidAttemptOutcome { .. } => "plan_attempt_outcome_invalid",
        PlanError::EmptyDecomposition | PlanError::NestedDecomposition => {
            "plan_decomposition_invalid"
        }
        PlanError::NestedPlanInput => "plan_nested_input",
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
    fn blocked_plan_recovery_explains_higher_budget_replacement() {
        let rejection = plan_error_rejection(&PlanError::WrongPhase {
            actual: PlanPhase::Blocked,
            operation: "replace subtree",
        });
        assert_eq!(rejection.code, "plan_wrong_phase");
        assert_eq!(
            rejection.recovery["next_action"],
            "inspect_and_resume_blocked_execution"
        );
        assert_eq!(rejection.recovery["next_tool"], "read_plan");
        let instruction = rejection.recovery["instruction"]
            .as_str()
            .expect("recovery instruction should be text");
        assert!(instruction.contains("subagent_max_model_turns_reached"));
        assert!(instruction.contains("same authored plan_client_key"));
        assert!(instruction.contains("within the configured maximum"));
        assert!(instruction.contains("larger value for complex tasks"));
        assert!(instruction.contains("shared workspace"));
        assert!(instruction.contains("do not use update_plan.replace_subtree"));
    }

    #[test]
    fn update_plan_decode_path_identifies_outer_metadata_nested_in_change() {
        let pending = PendingToolCall::new(
            merry_core::ToolCallId::new("call-update-plan").expect("valid call id"),
            merry_core::ToolName::new(UPDATE_PLAN_TOOL_NAME).expect("valid tool name"),
            merry_core::ToolCallArguments::try_from(serde_json::json!({
                "change": {
                    "type": "define_plan",
                    "coordinator_node_id": null,
                    "expected_plan_revision": 0,
                    "root": {
                        "client_key": "root",
                        "objective": "Complete the work",
                        "acceptance": ["Checks pass"],
                        "depends_on": [],
                        "children": []
                    }
                }
            }))
            .expect("object arguments"),
        );

        assert_eq!(input_decode_path(&pending), "change.coordinator_node_id");
        let error = input_from_call::<UpdatePlanInput>(&pending)
            .expect_err("nested coordinator metadata should be rejected");
        assert_eq!(error.path, "change.coordinator_node_id");
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

    #[test]
    fn active_linked_subtree_recovery_tells_coordinator_to_wait_or_create_new_assignment() {
        let node_id = merry_core::PlanNodeId::new("plan-node-active").expect("valid node id");
        let rejection = plan_error_rejection(&PlanError::ActiveSubagentOwnsSubtree { node_id });

        assert_eq!(rejection.code, "plan_active_subagent_owns_subtree");
        assert_eq!(
            rejection.recovery["next_action"],
            "wait_or_cancel_and_create_new_assignment"
        );
        let instruction = rejection.recovery["instruction"]
            .as_str()
            .expect("recovery instruction should be text");
        assert!(instruction.contains("active linked child"));
        assert!(instruction.contains("wait"));
        assert!(instruction.contains("cancel"));
        assert!(instruction.contains("new assignment"));
    }

    #[test]
    fn completed_node_recovery_uses_update_plan_without_exposing_runtime_identity() {
        let node_id = merry_core::PlanNodeId::new("plan-node-complete").expect("valid node id");
        let rejection = plan_error_rejection(&PlanError::NodeNotMutable {
            node_id,
            status: PlanNodeStatus::Verifying,
        });

        assert_eq!(rejection.code, "plan_node_not_mutable");
        assert_eq!(rejection.recovery["next_tool"], "update_plan");
        let instruction = rejection.recovery["instruction"]
            .as_str()
            .expect("recovery instruction should be text");
        assert!(instruction.contains("fresh root"));
        assert!(instruction.contains("verification child"));
    }
}
