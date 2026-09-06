use crate::{
    plan::projection::{CHILD_LINKED_SCOPE_GUIDANCE, LINKED_CHILD_DECOMPOSITION_GUIDANCE},
    subagent::{
        CANCEL_SUBAGENTS_TOOL_NAME, SPAWN_SUBAGENTS_TOOL_NAME, WAIT_SUBAGENTS_TOOL_NAME,
        protocol::{CancelSubagentsInput, SpawnSubagentsInput, WaitSubagentsInput},
    },
};
use merry_core::{ToolInputSchema, ToolSpec};
use serde_json::Value;

pub(super) const WAIT_SEMANTIC_CHECKPOINT_GUIDANCE: &str = "Observe child statuses at semantic or terminal checkpoints; do not poll for high-frequency progress.";
const CHILD_MODEL_TURN_GUIDANCE: &str = "max_model_turns covers the full child lifecycle, including tools, implementation, tests, verification, and reporting. Use enough turns for the task and never exceed the configured maximum. Omit it to use the configured default. If the limit is reached, the child is blocked but recoverable: inspect wait_subagents and spawn a replacement with the same plan_client_key and a larger budget within the configured maximum, continuing from the shared workspace.";

/// Returns provider-visible subagent tool specs.
pub fn subagent_tool_specs() -> Result<[ToolSpec; 3], merry_core::CoreError> {
    subagent_tool_specs_with_bounds(
        crate::subagent::DEFAULT_MIN_MODEL_TURNS,
        crate::subagent::DEFAULT_MAX_MODEL_TURNS,
    )
}

pub(super) fn subagent_tool_specs_with_bounds(
    min_model_turns: u32,
    max_model_turns: u32,
) -> Result<[ToolSpec; 3], merry_core::CoreError> {
    let spawn_description = format!(
        "Spawn bounded child agents for parallel delegated tasks. A terminal child result is delivered to the parent as a runtime update on the next model turn; it does not interrupt the current turn. Review that update before claiming the parent task is complete, and call wait_subagents when the compact result, diagnostics, or changed paths are needed. {CHILD_MODEL_TURN_GUIDANCE} The configured child model-turn range is {min_model_turns}..={max_model_turns}; explicit budgets outside it are rejected. Omit max_model_turns to use the configured default. Use the structured scope fields when a child needs narrower boundaries; keep tasks[].task focused on the work and do not repeat scope declarations in task text. When binding a child to an authored Plan node, set plan_client_key to one of the authored strings in update_plan.bindable_plan_client_keys, such as `agent1_task`; never pass a runtime node id such as `plan-node-2`. When plan_client_key binds a child: {CHILD_LINKED_SCOPE_GUIDANCE} {LINKED_CHILD_DECOMPOSITION_GUIDANCE} In tasks[].allowed_tools, copy exact registered Merry tool names without provider namespace prefixes: use run_process, never functions.run_process."
    );
    let spawn_spec =
        SpawnSubagentsInput::tool_spec_with(SPAWN_SUBAGENTS_TOOL_NAME, &spawn_description)?;
    let mut spawn_schema = spawn_spec.input_schema().as_schema().clone();
    if !set_model_turns_schema_bounds(&mut spawn_schema, min_model_turns, max_model_turns) {
        return Err(merry_core::CoreError::InvalidSchema {
            kind: "SpawnSubagentsInput",
            reason: "generated schema is missing the max_model_turns property",
        });
    }
    let spawn_spec = spawn_spec.with_input_schema(ToolInputSchema::new(spawn_schema)?)?;

    Ok([
        spawn_spec,
        WaitSubagentsInput::tool_spec_with(
            WAIT_SUBAGENTS_TOOL_NAME,
            format!(
                "Inspect or wait for child agent statuses and compact results. {WAIT_SEMANTIC_CHECKPOINT_GUIDANCE} timeout_ms is an observation deadline, not a task budget; zero returns an immediate status snapshot, while omission waits for the selected completion condition. A timed_out=true result is only a status snapshot, never completion. Claim completion only when terminal=true and the relevant statuses are terminal."
            ),
        )?,
        CancelSubagentsInput::tool_spec_with(
            CANCEL_SUBAGENTS_TOOL_NAME,
            "Cancel selected child agents.",
        )?,
    ])
}

fn set_model_turns_schema_bounds(
    schema: &mut schemars::Schema,
    min_model_turns: u32,
    max_model_turns: u32,
) -> bool {
    set_model_turns_schema_bounds_in_value(schema.as_object_mut(), min_model_turns, max_model_turns)
}

fn set_model_turns_schema_bounds_in_value(
    object: Option<&mut serde_json::Map<String, Value>>,
    min_model_turns: u32,
    max_model_turns: u32,
) -> bool {
    let Some(object) = object else {
        return false;
    };

    let mut found = false;
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut)
        && let Some(field) = properties
            .get_mut("max_model_turns")
            .and_then(Value::as_object_mut)
    {
        field.insert("minimum".to_owned(), serde_json::json!(min_model_turns));
        field.insert("default".to_owned(), serde_json::json!(max_model_turns));
        field.insert("maximum".to_owned(), serde_json::json!(max_model_turns));
        found = true;
    }

    for child in object.values_mut() {
        if set_model_turns_schema_bounds_in_value(
            child.as_object_mut(),
            min_model_turns,
            max_model_turns,
        ) {
            found = true;
        }
    }
    found
}
