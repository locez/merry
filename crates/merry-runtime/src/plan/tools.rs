//! Intrinsic coordinator Plan Mode tool definitions.

use crate::{
    RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionError, ToolExecutor,
    ToolExecutorFuture,
};
use merry_core::{CoreError, PlanId, ToolName, ToolSpec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub(crate) const ENTER_PLAN_MODE_TOOL_NAME: &str = "enter_plan_mode";
pub(crate) const WRITE_PLAN_TOOL_NAME: &str = "write_plan";
pub(crate) const READ_PLAN_TOOL_NAME: &str = "read_plan";
pub(crate) const EXIT_PLAN_MODE_TOOL_NAME: &str = "exit_plan_mode";

pub(crate) const COORDINATOR_PLAN_TOOL_NAMES: [&str; 4] = [
    ENTER_PLAN_MODE_TOOL_NAME,
    WRITE_PLAN_TOOL_NAME,
    READ_PLAN_TOOL_NAME,
    EXIT_PLAN_MODE_TOOL_NAME,
];

/// Input for `enter_plan_mode`: no parameters required.
#[merry_tools_macros::tool(
    crate = "crate",
    name = "enter_plan_mode",
    description = "Enter read-only Plan Mode to explore the codebase and design an implementation plan before editing workspace files."
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnterPlanModeInput {}

/// Input for `write_plan`: creates or updates a named Markdown plan document in the session.
#[merry_tools_macros::tool(
    crate = "crate",
    name = "write_plan",
    description = "Write or update a Markdown implementation plan stored in the session plans directory."
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WritePlanInput {
    /// Short, descriptive title for the plan (used for the file slug and UI header).
    #[schemars(description = "Short, descriptive title for the plan (max 200 UTF-8 bytes).")]
    pub(crate) title: String,
    /// Full Markdown body of the plan (Context, Recommended Approach, Key Files, Verification).
    #[schemars(
        description = "Complete Markdown content for the plan. Include Context, Approach, Key Files, and Verification."
    )]
    pub(crate) content: String,
    /// Optional existing draft plan id to update explicitly. Omit to update the active draft or create a new plan.
    #[serde(default)]
    #[schemars(
        description = "Optional existing draft plan id to update. Omit to update the current draft or create a new plan."
    )]
    pub(crate) plan_id: Option<PlanId>,
    /// Set to true to force creating a separate numbered plan instead of updating the current draft.
    #[serde(default)]
    #[schemars(
        description = "Optional. Set true to create a brand-new numbered plan file instead of updating the active draft."
    )]
    pub(crate) create_new: bool,
}

/// Input for `read_plan`: lists session plans and reads the requested or active plan body.
#[merry_tools_macros::tool(
    crate = "crate",
    name = "read_plan",
    description = "List plans saved in this session and read the Markdown content of the active or specified plan."
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadPlanInput {
    /// Optional plan id to read. Omit to read the active or most recent plan.
    #[serde(default)]
    #[schemars(description = "Optional plan id to read. Omit to read the active or latest plan.")]
    pub(crate) plan_id: Option<PlanId>,
}

/// Input for `exit_plan_mode`: submits the plan for user review and pauses execution until decided.
#[merry_tools_macros::tool(
    crate = "crate",
    name = "exit_plan_mode",
    description = "Submit your Markdown plan for user review and exit Plan Mode upon approval."
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExitPlanModeInput {
    /// Optional plan id to submit. Omit to submit the active draft plan.
    #[serde(default)]
    #[schemars(description = "Optional plan id to submit. Omit to submit the active draft plan.")]
    pub(crate) plan_id: Option<PlanId>,
}

pub(crate) fn coordinator_plan_registered_tools() -> Result<Vec<RegisteredTool>, CoreError> {
    let definitions = [
        intrinsic_tool(plan_tool_spec(EnterPlanModeInput::tool_spec_with(
            ENTER_PLAN_MODE_TOOL_NAME,
            "Enter read-only Plan Mode when a task has genuine architectural ambiguity or when the user asks for a plan first. While active, workspace file edits are blocked; explore read-only, author your plan with `write_plan`, and call `exit_plan_mode` for user review. Skip Plan Mode for straightforward changes.",
        ))?),
        intrinsic_tool(plan_tool_spec(WritePlanInput::tool_spec_with(
            WRITE_PLAN_TOOL_NAME,
            "Create or update a Markdown implementation plan in the session's `plans/` directory. Each distinct plan receives a numbered file name (`001-<slug>.md`, `002-<slug>.md`, ...). Updating during the same planning cycle updates the active draft in place unless `create_new: true` is set.",
        ))?),
        intrinsic_tool(plan_tool_spec(ReadPlanInput::tool_spec_with(
            READ_PLAN_TOOL_NAME,
            "List all Markdown plans saved in this session (with ids, titles, statuses, and file names) and return the full Markdown content of the active or requested plan.",
        ))?),
        intrinsic_tool(plan_tool_spec(ExitPlanModeInput::tool_spec_with(
            EXIT_PLAN_MODE_TOOL_NAME,
            "Finish planning and present the active Markdown plan to the user for approval. The interactive session pauses so the user can approve, attach review comments and request changes, or abandon the plan.",
        ))?),
    ];
    Ok(definitions.into_iter().collect())
}

pub(crate) fn is_plan_tool(name: &ToolName) -> bool {
    COORDINATOR_PLAN_TOOL_NAMES.contains(&name.as_str())
}

fn intrinsic_tool(spec: ToolSpec) -> RegisteredTool {
    RegisteredTool::new(
        spec,
        Arc::new(IntrinsicPlanExecutor),
        ToolActionKind::RuntimeControl,
    )
}

fn plan_tool_spec(result: Result<ToolSpec, crate::ToolBuildError>) -> Result<ToolSpec, CoreError> {
    result.map_err(CoreError::from)
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
