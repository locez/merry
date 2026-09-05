use super::{ReadPlanInput, SubagentPlanUpdateInput, UpdatePlanInput};
use crate::plan::projection::{
    CHILD_LINKED_SCOPE_GUIDANCE, CHILD_SCOPED_UPDATE_GUIDANCE,
    COORDINATOR_ACTIVE_LINK_MUTATION_GUIDANCE, COORDINATOR_LINKED_SUMMARIES_GUIDANCE,
    COORDINATOR_ROOT_SCOPE_GUIDANCE, LINKED_CHILD_DECOMPOSITION_GUIDANCE,
    RUNTIME_OWNED_EXECUTION_GUIDANCE,
};
use crate::{
    RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionError, ToolExecutor,
    ToolExecutorFuture,
};
use merry_core::{CoreError, ToolInputSchema, ToolName, ToolSpec};
use schemars::JsonSchema;
use std::sync::Arc;

pub(crate) const READ_PLAN_TOOL_NAME: &str = "read_plan";
pub(crate) const UPDATE_PLAN_TOOL_NAME: &str = "update_plan";

pub(crate) const COORDINATOR_PLAN_TOOL_NAMES: [&str; 2] =
    [READ_PLAN_TOOL_NAME, UPDATE_PLAN_TOOL_NAME];

pub(crate) fn coordinator_plan_registered_tools() -> Result<Vec<RegisteredTool>, CoreError> {
    let definitions = [
        plan_tool::<ReadPlanInput>(
            READ_PLAN_TOOL_NAME,
            format!(
                "Read a bounded exact snapshot or subtree of the current durable plan, including runtime-owned linked subagent summaries. {COORDINATOR_ROOT_SCOPE_GUIDANCE} {LINKED_CHILD_DECOMPOSITION_GUIDANCE} {COORDINATOR_LINKED_SUMMARIES_GUIDANCE} {COORDINATOR_ACTIVE_LINK_MUTATION_GUIDANCE} Historical attempt, lease, heartbeat, and model-report records are not part of this coordinator interface."
            ),
        )?,
        plan_tool::<UpdatePlanInput>(
            UPDATE_PLAN_TOOL_NAME,
            format!(
                "Create or update the durable Plan with a tagged JSON change object. The change argument must contain a nested string field change.type; valid coordinator values are define_plan, replace_subtree, and use_current_plan. Do not put type on the outer update object. Keep reason, execution_intent, coordinator_node_id, and max_concurrency_hint as top-level siblings of change; do not nest them inside change. The first valid update creates the Plan. A define_plan change describes the whole authored tree; when a fresh run is requested, use it again with a new root and direct children, without target_node_id or other runtime ids. Runtime archives the previous plan and owns the new plan and node identities. A successful result includes bindable_plan_client_keys: the authored client_key values that may be passed to spawn_subagents.tasks[].plan_client_key. It intentionally omits runtime node ids; call read_plan when an exact node id is needed for replace_subtree or an existing-node dependency. {COORDINATOR_ROOT_SCOPE_GUIDANCE} {LINKED_CHILD_DECOMPOSITION_GUIDANCE} {COORDINATOR_LINKED_SUMMARIES_GUIDANCE} {COORDINATOR_ACTIVE_LINK_MUTATION_GUIDANCE} New nodes use client_key without id; existing mutable nodes use id without client_key. The root and each node's direct children may be authored in one request; omit children and depends_on when they are empty. Do not nest a child's implementation subtree in coordinator input. Define or revise authored intent, dependencies, and acceptance; runtime-owned execution state is derived from actual activity. If acceptance needs an explicit check, author a direct verification child; runtime closes the root after every declared child completes. Use execute_if_authorized only when the user already authorized execution, and request_user_review only when an explicit review boundary is wanted. When the Plan is already executing, do not call use_current_plan again; continue ordinary work or revise only a genuinely changed future subtree."
            ),
        )?,
    ];
    Ok(definitions.into_iter().collect())
}

pub(crate) fn subagent_plan_registered_tools() -> Result<Vec<RegisteredTool>, CoreError> {
    unbound_child_plan_registered_tools()
}

pub(crate) fn scoped_child_plan_registered_tools() -> Result<Vec<RegisteredTool>, CoreError> {
    let definitions = [
        plan_tool::<ReadPlanInput>(
            READ_PLAN_TOOL_NAME,
            format!(
                "Read a bounded exact snapshot or subtree below the linked task in the active Plan. {CHILD_LINKED_SCOPE_GUIDANCE} {CHILD_SCOPED_UPDATE_GUIDANCE} This linked subtree scope excludes the coordinator and sibling tasks. {RUNTIME_OWNED_EXECUTION_GUIDANCE} Historical attempt, lease, heartbeat, and model-report records are not part of this child interface."
            ),
        )?,
        plan_tool::<SubagentPlanUpdateInput>(
            UPDATE_PLAN_TOOL_NAME,
            format!(
                "Update authored children or replace a mutable subtree below the linked task in the active Plan. The change argument must contain a nested string field change.type; valid child values are define_children and replace_subtree. Do not put type on the outer update object. {CHILD_LINKED_SCOPE_GUIDANCE} {CHILD_SCOPED_UPDATE_GUIDANCE} This linked subtree scope excludes the coordinator and sibling tasks; {RUNTIME_OWNED_EXECUTION_GUIDANCE} Child binding identity remains controlled by the runtime."
            ),
        )?,
    ];
    Ok(definitions.into_iter().collect())
}

pub(crate) fn unbound_child_plan_registered_tools() -> Result<Vec<RegisteredTool>, CoreError> {
    Ok(Vec::new())
}

pub(crate) fn is_plan_tool(name: &ToolName) -> bool {
    COORDINATOR_PLAN_TOOL_NAMES.contains(&name.as_str())
}

fn plan_tool<T>(name: &str, description: impl Into<String>) -> Result<RegisteredTool, CoreError>
where
    T: JsonSchema,
{
    let schema = ToolInputSchema::new(schemars::schema_for!(T))?;
    let description = description.into();
    let spec = ToolSpec::new(ToolName::new(name)?, &description, schema)?;
    Ok(RegisteredTool::new(
        spec,
        Arc::new(IntrinsicPlanExecutor),
        ToolActionKind::RuntimeControl,
    ))
}

struct IntrinsicPlanExecutor;

impl ToolExecutor for IntrinsicPlanExecutor {
    fn execute<'a>(
        &'a self,
        call: merry_core::PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            Err(ToolExecutionError::infrastructure(format!(
                "intrinsic plan tool {} reached the generic executor",
                call.name()
            )))
        })
    }
}
