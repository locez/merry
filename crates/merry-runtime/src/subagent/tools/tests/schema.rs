use super::{CapturingChildFactory, manager};
use crate::{
    SubagentConfig, SubagentManager, ToolActionKind,
    schema_contract::assert_provider_input_schema_fields_have_descriptions,
    subagent::tools::{subagent_registered_tools, subagent_tool_specs},
    subagent::{
        CANCEL_SUBAGENTS_TOOL_NAME, SEND_SUBAGENT_MESSAGE_TOOL_NAME, SPAWN_SUBAGENTS_TOOL_NAME,
        WAIT_SUBAGENTS_TOOL_NAME,
    },
};
use merry_core::SessionId;
use serde_json::{Value, json};
use std::sync::Arc;

#[test]
fn spawn_schema_exposes_roles_context_files_and_worktree() {
    let specs = subagent_tool_specs().expect("subagent tools build");
    let schema = serde_json::to_value(specs[0].input_schema()).expect("spawn schema serializes");
    assert!(schema.to_string().contains("context_files"));
    assert!(schema.to_string().contains("role"));
    assert!(schema.to_string().contains("worktree_base"));
    assert!(specs[0].description().contains("worktree"));
}

#[test]
fn wait_schema_accepts_zero_deadline_as_status_snapshot() {
    let specs = subagent_tool_specs().expect("subagent tools build");
    let schema = serde_json::to_value(specs[2].input_schema()).expect("wait schema serializes");
    assert!(schema.to_string().contains("minimum"));
    assert!(schema.to_string().contains("\"minimum\":0"));
    assert!(specs[2].description().contains("observation deadline"));

    let validator = jsonschema::validator_for(specs[2].input_schema().as_schema().as_value())
        .expect("wait schema compiles");
    assert!(validator.is_valid(&json!({
        "agent_ids": ["agent-1"],
        "timeout_ms": 0
    })));
    assert!(
        specs[2]
            .description()
            .contains(crate::subagent::tools::schema::WAIT_SEMANTIC_CHECKPOINT_GUIDANCE)
    );
    assert!(
        !specs[2]
            .description()
            .contains("separate UI activity stream")
    );
}

#[test]
fn provider_visible_subagent_schemas_describe_fields_and_match_runtime_bounds() {
    let specs = subagent_tool_specs().expect("subagent tools build");
    for spec in &specs {
        assert_provider_input_schema_fields_have_descriptions(spec);
    }

    let spawn_schema = specs[0].input_schema().as_schema().as_value();
    let spawn_validator = jsonschema::validator_for(spawn_schema).expect("spawn schema compiles");
    let valid_task = json!({
        "tasks": [{
            "task": "Review the runtime.",
            "role": "explore",
            "max_model_turns": 20,
            "read_scope": ["crates/merry-runtime"],
            "write_scope": ["tmp/output"],
            "context_files": ["crates/merry-runtime/src/lib.rs:1-40"]
        }]
    });
    if let Err(error) = spawn_validator.validate(&valid_task) {
        panic!("valid task rejected by schema: {error}");
    }

    assert_eq!(
        find_model_turns_schema(spawn_schema).and_then(|field| field["minimum"].as_u64()),
        Some(1)
    );

    let mut zero_turns = valid_task.clone();
    zero_turns["tasks"][0]["max_model_turns"] = json!(0);
    assert!(!spawn_validator.is_valid(&zero_turns));

    let mut above_default = valid_task.clone();
    above_default["tasks"][0]["max_model_turns"] = json!(1025);
    assert!(!spawn_validator.is_valid(&above_default));

    let mut oversized_task = valid_task.clone();
    oversized_task["tasks"][0]["task"] = json!("x".repeat(16 * 1024 + 1));
    assert!(!spawn_validator.is_valid(&oversized_task));

    let mut invalid_scope = valid_task;
    invalid_scope["tasks"][0]["read_scope"] = json!(["../outside"]);
    assert!(!spawn_validator.is_valid(&invalid_scope));

    for spec in &specs[2..] {
        let validator = jsonschema::validator_for(spec.input_schema().as_schema().as_value())
            .expect("status operation schema compiles");
        assert!(!validator.is_valid(&json!({ "agent_ids": [] })));
    }
}

#[test]
fn subagent_tool_descriptions_explain_slimmed_contract_and_anti_micromanagement() {
    let specs = subagent_tool_specs().expect("subagent tools build");
    assert!(specs[0].description().contains("<subagent-notification>"));
    assert!(specs[0].description().contains("send_subagent_message"));
    assert!(specs[0].description().contains("explore"));
    assert!(specs[0].description().contains("coder"));
    assert!(specs[0].description().contains("review"));
    assert!(specs[1].description().contains("Do NOT send status checks"));
    assert!(
        specs[2]
            .description()
            .contains("semantic or terminal checkpoints")
    );
    let schema = serde_json::to_string(specs[0].input_schema()).expect("schema serializes");
    assert!(schema.contains("Empty array makes the child read-only"));
}

#[test]
fn registered_subagent_tools_are_named_with_control_policy() {
    let tools = subagent_registered_tools(manager(Arc::new(CapturingChildFactory::default())))
        .expect("registered subagent tools should build");

    assert_eq!(tools.len(), 4);
    assert_eq!(tools[0].spec().name().as_str(), SPAWN_SUBAGENTS_TOOL_NAME);
    assert_eq!(
        tools[1].spec().name().as_str(),
        SEND_SUBAGENT_MESSAGE_TOOL_NAME
    );
    assert_eq!(tools[2].spec().name().as_str(), WAIT_SUBAGENTS_TOOL_NAME);
    assert_eq!(tools[3].spec().name().as_str(), CANCEL_SUBAGENTS_TOOL_NAME);
    assert_eq!(tools[0].action_kind(), ToolActionKind::RuntimeControl);
    assert_eq!(tools[1].action_kind(), ToolActionKind::RuntimeControl);
    assert_eq!(tools[2].action_kind(), ToolActionKind::ReadOnly);
    assert_eq!(tools[3].action_kind(), ToolActionKind::RuntimeControl);
}

#[test]
fn registered_subagent_schema_uses_manager_configured_model_turn_default() {
    let config = SubagentConfig::default()
        .with_max_model_turns(2048)
        .expect("positive child model-turn limit");
    let manager = SubagentManager::new(
        SessionId::new("configured-schema").expect("valid session id"),
        config,
        Arc::new(CapturingChildFactory::default()),
    );
    let tools = subagent_registered_tools(manager).expect("registered tools should build");
    let schema =
        serde_json::to_value(tools[0].spec().input_schema()).expect("spawn schema serializes");

    assert_eq!(
        find_model_turns_schema(&schema).and_then(|field| field["minimum"].as_u64()),
        Some(1)
    );
    assert_eq!(
        find_model_turns_schema(&schema).and_then(|field| field["default"].as_u64()),
        Some(2048)
    );
    assert_eq!(
        find_model_turns_schema(&schema).and_then(|field| field["maximum"].as_u64()),
        Some(2048)
    );
    assert!(
        tools[0]
            .spec()
            .description()
            .contains("Configured child model-turn range: 1..=2048")
    );
}

#[test]
fn registered_subagent_schema_uses_configured_model_turn_bounds() {
    let config = SubagentConfig::default()
        .with_model_turn_bounds(2048, 4096)
        .expect("test model-turn bounds should be valid");
    let manager = SubagentManager::new(
        SessionId::new("configured-bounds").expect("valid session id"),
        config,
        Arc::new(CapturingChildFactory::default()),
    );
    let tools = subagent_registered_tools(manager).expect("registered tools should build");
    let schema =
        serde_json::to_value(tools[0].spec().input_schema()).expect("spawn schema serializes");

    assert_eq!(
        find_model_turns_schema(&schema).and_then(|field| field["minimum"].as_u64()),
        Some(2048)
    );
    assert_eq!(
        find_model_turns_schema(&schema).and_then(|field| field["default"].as_u64()),
        Some(4096)
    );
    assert_eq!(
        find_model_turns_schema(&schema).and_then(|field| field["maximum"].as_u64()),
        Some(4096)
    );
}

fn find_model_turns_schema(value: &Value) -> Option<&Value> {
    let object = value.as_object()?;
    object
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get("max_model_turns"))
        .or_else(|| object.values().find_map(find_model_turns_schema))
}
