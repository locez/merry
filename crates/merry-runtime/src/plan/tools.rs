use super::{ReadPlanInput, SubagentPlanUpdateInput, UpdatePlanInput};
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
            "Read a bounded exact snapshot or subtree of the current durable plan, including runtime-owned linked subagent summaries. Historical attempt, lease, heartbeat, and model-report records are not part of this coordinator interface.",
        )?,
        plan_tool::<UpdatePlanInput>(
            UPDATE_PLAN_TOOL_NAME,
            "Create or update the durable Plan with a tagged JSON change object. The first valid update creates the Plan. New nodes use client_key without id; existing mutable nodes use id without client_key. Define or revise authored intent, dependencies, and acceptance; runtime-owned execution state is derived from actual activity. Use execute_if_authorized only when the user already authorized execution, and request_user_review only when an explicit review boundary is wanted. When the Plan is already executing, do not call use_current_plan again; continue ordinary work or revise only a genuinely changed future subtree.",
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
            "Read a bounded exact snapshot or subtree below the linked task in the active Plan. This linked subtree scope excludes the coordinator and sibling tasks; historical attempt, lease, heartbeat, and model-report records are not part of this child interface.",
        )?,
        plan_tool::<SubagentPlanUpdateInput>(
            UPDATE_PLAN_TOOL_NAME,
            "Update authored children or replace a mutable subtree below the linked task in the active Plan. This linked subtree scope excludes the coordinator and sibling tasks; runtime-owned execution state and child binding identity remain controlled by the runtime.",
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

fn plan_tool<T>(name: &str, description: &str) -> Result<RegisteredTool, CoreError>
where
    T: JsonSchema,
{
    let schema = ToolInputSchema::new(schemars::schema_for!(T))?;
    let spec = ToolSpec::new(ToolName::new(name)?, description, schema)?;
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
