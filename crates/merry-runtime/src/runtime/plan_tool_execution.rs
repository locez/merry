use crate::{
    ArtifactContent, PlanControllerError, PlanError, RuntimeError,
    plan::{
        BeginPlanInput, ReadPlanInput, UpdatePlanInput,
        tools::{BEGIN_PLAN_TOOL_NAME, READ_PLAN_TOOL_NAME, UPDATE_PLAN_TOOL_NAME},
    },
    tool::ToolExecutionContext,
};
use merry_core::{
    ErrorInfo, PendingToolCall, PlanNodeId, PlanSnapshot, RuntimeJournalEvent, ToolCallResultStatus,
};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;

use super::{RuntimeInner, persist_resume_safe_savepoint_if_configured};

const PLAN_READ_MAX_DEPTH: u8 = 16;
const PLAN_READ_ATTEMPT_PAGE_SIZE: usize = 32;
const PLAN_READ_AUXILIARY_LIMIT: usize = 32;

pub(super) async fn execute_plan_tool_call(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    _context: ToolExecutionContext,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    match pending.name().as_str() {
        BEGIN_PLAN_TOOL_NAME => {
            let input = input_from_call::<BeginPlanInput>(inner, pending)?;
            match inner
                .plan_controller
                .begin_from_tool(input, pending.id().clone())
                .await
            {
                Ok(events) => Ok(events),
                Err(error) => submit_controller_error(inner, pending, error).await,
            }
        }
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
    snapshot: PlanSnapshot,
    selected_node_id: Option<PlanNodeId>,
    next_cursor: Option<String>,
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
                .ok_or_else(|| PlanToolRejection::new("no_active_plan", "no active plan exists"))?,
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

    let history_node_ids =
        selected_ids.unwrap_or_else(|| snapshot.nodes.iter().map(|node| node.id.clone()).collect());
    let attempt_offset = parse_attempt_cursor(input.cursor.as_deref())?;
    let mut next_cursor = None;
    if input.include_attempts.unwrap_or(false) {
        snapshot
            .attempts
            .retain(|attempt| history_node_ids.contains(&attempt.node_id));
        snapshot.attempts.sort_by(|left, right| {
            left.started_at_ms
                .cmp(&right.started_at_ms)
                .then_with(|| left.attempt_id.cmp(&right.attempt_id))
        });
        if attempt_offset > snapshot.attempts.len() {
            return Err(PlanToolRejection::new(
                "plan_read_cursor_invalid",
                "attempt cursor is beyond the available history",
            ));
        }
        let attempt_count = snapshot.attempts.len();
        let end = attempt_offset
            .saturating_add(PLAN_READ_ATTEMPT_PAGE_SIZE)
            .min(attempt_count);
        let page = snapshot.attempts[attempt_offset..end].to_vec();
        let page_attempt_ids = page
            .iter()
            .map(|attempt| attempt.attempt_id.clone())
            .collect::<BTreeSet<_>>();
        snapshot.attempts = page;
        snapshot
            .leases
            .retain(|lease| page_attempt_ids.contains(&lease.attempt_id));
        if end < attempt_count {
            next_cursor = Some(format!("attempts:{end}"));
        }
    } else {
        if attempt_offset != 0 {
            return Err(PlanToolRejection::new(
                "plan_read_cursor_invalid",
                "attempt cursor requires include_attempts=true",
            ));
        }
        snapshot.attempts.clear();
        snapshot.leases.clear();
    }

    if input.include_progress.unwrap_or(false) {
        snapshot
            .attempt_progress
            .retain(|progress| history_node_ids.contains(&progress.node_id));
        keep_latest(
            &mut snapshot.attempt_progress,
            PLAN_READ_AUXILIARY_LIMIT,
            |progress| progress.last_runtime_activity_at_ms,
        );
    } else {
        snapshot.attempt_progress.clear();
    }
    if input.include_directives.unwrap_or(false) {
        snapshot
            .directives
            .retain(|directive| history_node_ids.contains(&directive.node_id));
        keep_latest(
            &mut snapshot.directives,
            PLAN_READ_AUXILIARY_LIMIT,
            |directive| directive.sequence,
        );
    } else {
        snapshot.directives.clear();
    }

    Ok(ReadPlanOutput {
        snapshot,
        selected_node_id: input.node_id,
        next_cursor,
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

fn parse_attempt_cursor(cursor: Option<&str>) -> Result<usize, PlanToolRejection> {
    match cursor {
        None => Ok(0),
        Some(cursor) => cursor
            .strip_prefix("attempts:")
            .and_then(|offset| offset.parse::<usize>().ok())
            .ok_or_else(|| {
                PlanToolRejection::new(
                    "plan_read_cursor_invalid",
                    "cursor must use the attempts:<offset> format returned by read_plan",
                )
            }),
    }
}

fn keep_latest<T>(items: &mut Vec<T>, limit: usize, key: impl Fn(&T) -> u64) {
    items.sort_by_key(&key);
    if items.len() > limit {
        items.drain(..items.len() - limit);
    }
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
            submit_rejection(
                inner,
                pending,
                PlanToolRejection::new(plan_error_code(&source), source.to_string()),
            )
            .await
        }
        PlanControllerError::NoActivePlan => {
            submit_rejection(
                inner,
                pending,
                PlanToolRejection::new("no_active_plan", error.to_string()),
            )
            .await
        }
    }
}

async fn submit_rejection(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    rejection: PlanToolRejection,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let diagnostic =
        ErrorInfo::new(rejection.code, &rejection.message).map_err(RuntimeError::from)?;
    let content = serde_json::json!({
        "ok": false,
        "tool": pending.name().as_str(),
        "error": {
            "code": rejection.code,
            "message": rejection.message,
        },
        "recovery": {
            "read_plan": "Read the latest exact plan state before retrying a revision-sensitive operation."
        }
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
}

impl PlanToolRejection {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn plan_error_code(error: &PlanError) -> &'static str {
    match error {
        PlanError::InvalidText { .. } => "plan_invalid_text",
        PlanError::WrongPhase { .. } => "plan_wrong_phase",
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
        PlanError::InvalidScopePath { .. } | PlanError::CapabilityEnvelopeExceeded { .. } => {
            "plan_capability_envelope_exceeded"
        }
        PlanError::NodeNotMutable { .. } => "plan_node_not_mutable",
        PlanError::ReplacementRootIdentity { .. } => "plan_invalid_replacement_root",
        PlanError::InvalidConcurrencyHint { .. } => "plan_invalid_concurrency_hint",
        PlanError::InvalidPersistedCounters => "plan_persisted_state_invalid",
    }
}
