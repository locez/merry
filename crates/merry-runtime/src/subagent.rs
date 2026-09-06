//! Runtime-owned parallel subagent tool contracts.

mod activity;
mod child;
mod manager;
mod plan_link;
mod protocol;
mod scheduler;
mod scope;
mod spec;
mod tools;

pub use activity::{SubagentActivityHub, SubagentActivityReceiver};
pub(crate) use child::sanitize_diagnostic_message;
pub use manager::SubagentManager;
pub(crate) use plan_link::plan_link_runtime_for_controller;
pub use plan_link::{PlanLinkRuntime, PlanSubagentScope};
pub use protocol::{
    CancelSubagentsInput, RejectedSubagentView, SpawnSubagentTaskInput, SpawnSubagentsInput,
    SpawnSubagentsOutput, SpawnedSubagentStatusLabel, SpawnedSubagentView, SubagentResultView,
    SubagentStatusLabel, SubagentStatusView, WaitMode, WaitSubagentsInput, WaitSubagentsOutput,
};
pub use scope::{ChildRuntimeFactory, ChildRuntimeInput, ChildWorkspaceScope};
use spec::validate_task_max_model_turns;
pub use spec::{
    DEFAULT_MAX_MODEL_TURNS, DEFAULT_MIN_MODEL_TURNS, SubagentConfig, SubagentError,
    SubagentTaskSpec, validate_no_write_scope_conflicts,
};
pub use tools::{subagent_registered_tools, subagent_tool_specs};

/// Formats terminal child statuses as a model-visible runtime continuation.
pub(crate) fn completion_notification_text(statuses: &[SubagentStatusView]) -> String {
    let mut text = String::from(
        "Runtime update: child agents reached terminal states. Review these results and incorporate any required follow-up before claiming the parent task is complete:\n",
    );
    for status in statuses {
        text.push_str("- ");
        text.push_str(status.agent_id.as_str());
        text.push_str(" [");
        text.push_str(status.status.as_str());
        text.push_str("] ");
        text.extend(status.summary.chars().map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        }));
        text.push('\n');
    }
    text.push_str(
        "Call wait_subagents with the listed agent ids when the compact result, diagnostics, or changed paths are needed. A blocked child caused by max_model_turns_reached is recoverable: spawn a replacement with the same plan_client_key and a larger budget within the configured maximum, continuing from the shared workspace.",
    );
    text
}

/// Provider-visible tool name for spawning bounded child agents.
pub(crate) const SPAWN_SUBAGENTS_TOOL_NAME: &str = "spawn_subagents";
/// Provider-visible tool name for waiting on child agent statuses/results.
pub(crate) const WAIT_SUBAGENTS_TOOL_NAME: &str = "wait_subagents";
/// Provider-visible tool name for cancelling child agents.
pub(crate) const CANCEL_SUBAGENTS_TOOL_NAME: &str = "cancel_subagents";
/// Construction-context id for the runtime-derived effective workspace scope.
pub(crate) const SUBAGENT_WORKSPACE_SCOPE_CONTEXT_ID: &str = "subagent-workspace-scope";

const APPLY_PATCH_TOOL_NAME: &str = "apply_patch";
