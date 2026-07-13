use super::{
    BeginPlanInput, ControlPlanAttemptInput, ReadPlanInput, ReportPlanAttemptInput,
    ReportPlanProgressInput, UpdatePlanInput,
};
use crate::{
    RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionError, ToolExecutor,
    ToolExecutorFuture,
};
use merry_core::{CoreError, ToolInputSchema, ToolName, ToolSpec};
use schemars::JsonSchema;
use std::sync::Arc;

pub(crate) const BEGIN_PLAN_TOOL_NAME: &str = "begin_plan";
pub(crate) const READ_PLAN_TOOL_NAME: &str = "read_plan";
pub(crate) const UPDATE_PLAN_TOOL_NAME: &str = "update_plan";
pub(crate) const CONTROL_PLAN_ATTEMPT_TOOL_NAME: &str = "control_plan_attempt";
pub(crate) const REPORT_PLAN_PROGRESS_TOOL_NAME: &str = "report_plan_progress";
pub(crate) const REPORT_PLAN_ATTEMPT_TOOL_NAME: &str = "report_plan_attempt";

pub(crate) const COORDINATOR_PLAN_TOOL_NAMES: [&str; 6] = [
    BEGIN_PLAN_TOOL_NAME,
    READ_PLAN_TOOL_NAME,
    UPDATE_PLAN_TOOL_NAME,
    CONTROL_PLAN_ATTEMPT_TOOL_NAME,
    REPORT_PLAN_PROGRESS_TOOL_NAME,
    REPORT_PLAN_ATTEMPT_TOOL_NAME,
];

pub(crate) fn coordinator_plan_registered_tools() -> Result<Vec<RegisteredTool>, CoreError> {
    let definitions = [
        plan_tool::<BeginPlanInput>(
            BEGIN_PLAN_TOOL_NAME,
            "Activate durable Plan Mode for the current session without changing permissions.",
        )?,
        plan_tool::<ReadPlanInput>(
            READ_PLAN_TOOL_NAME,
            "Read a bounded exact snapshot or subtree of the current durable plan.",
        )?,
        plan_tool::<UpdatePlanInput>(
            UPDATE_PLAN_TOOL_NAME,
            "Define a complete planning tree or replace one mutable future subtree.",
        )?,
        plan_tool::<ControlPlanAttemptInput>(
            CONTROL_PLAN_ATTEMPT_TOOL_NAME,
            "Persist attempt-scoped status, steering, convergence, checkpoint, yield, or safe-cancel guidance.",
        )?,
        plan_tool::<ReportPlanProgressInput>(
            REPORT_PLAN_PROGRESS_TOOL_NAME,
            "Record bounded non-terminal semantic progress for the current local plan attempt.",
        )?,
        plan_tool::<ReportPlanAttemptInput>(
            REPORT_PLAN_ATTEMPT_TOOL_NAME,
            "Resolve the current local plan attempt exactly once with a typed result or decomposition.",
        )?,
    ];
    Ok(definitions.into_iter().collect())
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
