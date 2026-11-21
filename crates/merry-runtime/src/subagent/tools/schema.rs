use crate::subagent::{
    CANCEL_SUBAGENTS_TOOL_NAME, SEND_SUBAGENT_MESSAGE_TOOL_NAME, SPAWN_SUBAGENTS_TOOL_NAME,
    WAIT_SUBAGENTS_TOOL_NAME,
    protocol::{
        CancelSubagentsInput, SendSubagentMessageInput, SpawnSubagentsInput, WaitSubagentsInput,
    },
};
use merry_core::{ToolInputSchema, ToolSpec};
use serde_json::Value;

pub(super) const WAIT_SEMANTIC_CHECKPOINT_GUIDANCE: &str = "Observe child statuses at semantic or terminal checkpoints; do not poll for high-frequency progress.";
const CHILD_MODEL_TURN_GUIDANCE: &str = "Omit max_model_turns to use the configured default. If a child reaches its turn limit or times out, it stops as blocked with its transcript intact: resume that same child with send_subagent_message(agent_id, extra_model_turns=...) instead of respawning from scratch.";
const SEND_MESSAGE_ANTI_MICROMANAGE_GUIDANCE: &str = "Send a follow-up instruction to an active subagent or wake a completed, blocked, or cancelled subagent with its full prior transcript and agent_id intact. Use delivery=\"steer\" (default) to redirect at the next safe checkpoint or \"queue\" to append after buffered follow-ups, and set extra_model_turns when extending a blocked child. Do NOT send status checks, progress nudges, or \"hurry up / stop reading\" messages — use this only to supply new facts, correct a wrong target, extend a blocked child, or continue work with a finished child.";

/// Returns provider-visible subagent tool specs.
pub fn subagent_tool_specs() -> Result<[ToolSpec; 4], merry_core::CoreError> {
    subagent_tool_specs_with_bounds_and_roles(
        crate::subagent::DEFAULT_MIN_MODEL_TURNS,
        crate::subagent::DEFAULT_MAX_MODEL_TURNS,
        &[],
    )
}

pub(super) fn subagent_tool_specs_with_bounds_and_roles(
    min_model_turns: u32,
    max_model_turns: u32,
    role_summaries: &[(String, String)],
) -> Result<[ToolSpec; 4], merry_core::CoreError> {
    let roles_clause = if role_summaries.is_empty() {
        "Use `role` to pick a specialist (`explore` for fast read-only search, `coder` for scoped implementation and checks, `review` for independent skeptical verification) and `context_files` to preload key file slices before turn 1.".to_owned()
    } else {
        let list = role_summaries
            .iter()
            .map(|(name, desc)| format!("`{name}` ({desc})"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Use `role` to pick an enabled specialist role ({list}) and `context_files` to preload key file slices before turn 1."
        )
    };
    let spawn_description = format!(
        "Spawn one or more bounded child agents for independent, parallel, or context-heavy tasks. \
         Terminal child results are delivered automatically on the next model turn as a `<subagent-notification>` block with inlined conclusions and changed_paths; set `wait` to `all` or `any` only when you need results inline before continuing (if the foreground wait budget expires or the turn is interrupted, children keep running in the background — do not spawn duplicates). \
         {roles_clause} \
         {CHILD_MODEL_TURN_GUIDANCE} Configured child model-turn range: {min_model_turns}..={max_model_turns}. \
         Keep `tasks[].task` focused on the objective and ruled-out paths; declare path boundaries in `read_scope`, `write_scope`, and `forbidden_paths`. \
         When binding to a Plan node, pass an authored `plan_client_key` from `update_plan.bindable_plan_client_keys` (never a runtime node id such as `plan-node-2`). \
         In `tasks[].allowed_tools`, copy exact registered Merry tool names without provider namespace prefixes: use `run_process`, never `functions.run_process`."
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
        SendSubagentMessageInput::tool_spec_with(
            SEND_SUBAGENT_MESSAGE_TOOL_NAME,
            SEND_MESSAGE_ANTI_MICROMANAGE_GUIDANCE,
        )?,
        WaitSubagentsInput::tool_spec_with(
            WAIT_SUBAGENTS_TOOL_NAME,
            format!(
                "Inspect or wait for child agent statuses, stats, and full conclusions. {WAIT_SEMANTIC_CHECKPOINT_GUIDANCE} timeout_ms is an observation deadline, not a task budget; zero returns an immediate status snapshot, while omission waits for the selected completion condition. A timed_out=true result is only a status snapshot, never completion. Claim completion only when terminal=true and the relevant statuses are terminal."
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
