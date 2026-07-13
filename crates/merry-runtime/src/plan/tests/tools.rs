use crate::plan::{
    PlanChangeInput, PlanExecutionIntent, PlanNodeInput, UpdatePlanInput,
    tools::{
        CONTROL_PLAN_ATTEMPT_TOOL_NAME, COORDINATOR_PLAN_TOOL_NAMES, READ_PLAN_TOOL_NAME,
        REPORT_PLAN_ATTEMPT_TOOL_NAME, REPORT_PLAN_PROGRESS_TOOL_NAME, UPDATE_PLAN_TOOL_NAME,
        coordinator_plan_registered_tools,
    },
};
use merry_core::{PlanExecutorPolicy, PlanHarnessSnapshot, PlanRecoveryPolicySnapshot, ToolSpec};
use serde_json::{Value, json};

#[test]
fn coordinator_plan_tools_have_stable_names_schemas_and_runtime_control_policy() {
    let tools = coordinator_plan_registered_tools().expect("plan tools build");
    let names = tools
        .iter()
        .map(|tool| tool.spec().name().as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, COORDINATOR_PLAN_TOOL_NAMES);
    assert!(tools.iter().all(|tool| {
        tool.action_kind() == crate::ToolActionKind::RuntimeControl
            && tool.spec().input_schema().as_schema().as_object().is_some()
    }));
}

#[test]
fn update_plan_schema_rejects_missing_or_conflicting_node_identity() {
    let tool = update_plan_tool();
    let validator = jsonschema::validator_for(tool.input_schema().as_schema().as_value())
        .expect("update_plan schema compiles");
    assert!(validator.is_valid(&valid_update_arguments()));

    let mut missing = valid_update_arguments();
    missing
        .pointer_mut("/change/root")
        .and_then(Value::as_object_mut)
        .expect("root is an object")
        .remove("client_key");
    assert!(
        !validator.is_valid(&missing),
        "a new node without client_key must be rejected by the provider-visible schema"
    );

    let mut conflicting = valid_update_arguments();
    conflicting["change"]["root"]["id"] = json!("plan-node-1");
    assert!(
        !validator.is_valid(&conflicting),
        "a node with both id and client_key must be rejected by the provider-visible schema"
    );
}

#[test]
fn update_plan_schema_accepts_workspace_root_and_rejects_parent_traversal() {
    let tool = update_plan_tool();
    let validator = jsonschema::validator_for(tool.input_schema().as_schema().as_value())
        .expect("update_plan schema compiles");
    assert!(validator.is_valid(&valid_update_arguments()));
    let mut arguments = valid_update_arguments();
    arguments["change"]["root"]["harness"]["write_scope"] = json!(["."]);

    assert!(
        validator.is_valid(&arguments),
        "the provider-visible schema must accept the workspace root scope"
    );
    arguments["change"]["root"]["harness"]["write_scope"] = json!(["../outside"]);
    assert!(!validator.is_valid(&arguments));
}

#[test]
fn read_plan_schema_supports_explicit_lease_selection() {
    let tool = plan_tool(READ_PLAN_TOOL_NAME);
    let validator = jsonschema::validator_for(tool.input_schema().as_schema().as_value())
        .expect("read_plan schema compiles");

    assert!(validator.is_valid(&json!({
        "include_attempts": true,
        "include_leases": true,
        "include_progress": true
    })));
}

#[test]
fn attempt_reporting_schema_does_not_expose_runtime_owned_identity() {
    for name in [
        REPORT_PLAN_PROGRESS_TOOL_NAME,
        REPORT_PLAN_ATTEMPT_TOOL_NAME,
    ] {
        let schema = serde_json::to_value(plan_tool(name).input_schema())
            .expect("plan report schema serializes");
        let properties = schema
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("report schema has properties");
        assert!(
            !properties.contains_key("lease_id"),
            "{name} leaked lease_id"
        );
        assert!(
            !properties.contains_key("expected_node_revision"),
            "{name} leaked expected_node_revision"
        );
    }

    let schema = serde_json::to_value(plan_tool(CONTROL_PLAN_ATTEMPT_TOOL_NAME).input_schema())
        .expect("control schema serializes");
    let properties = schema
        .pointer("/properties")
        .and_then(Value::as_object)
        .expect("control schema has properties");
    assert!(!properties.contains_key("expected_lease_id"));
    assert!(!properties.contains_key("expected_node_revision"));
}

#[test]
fn update_plan_contract_explains_lifecycle_and_runtime_owned_state() {
    let tool = update_plan_tool();
    let description = tool.description();
    assert!(description.contains("begin_plan"));
    assert!(description.contains("client_key"));
    assert!(description.contains("runtime-owned"));
    assert!(description.contains("execute_if_authorized"));
    assert!(description.contains("use_current_plan"));
    assert!(description.contains("do not replace or recreate the tree"));

    let schema = serde_json::to_string(tool.input_schema()).expect("schema serializes");
    assert!(schema.contains("Call begin_plan before update_plan"));
    assert!(schema.contains("New nodes use client_key"));
    assert!(schema.contains("crates/merry-runtime"));
    assert!(schema.contains("already requested execution"));
    assert!(schema.contains("execute_if_authorized"));
    assert!(schema.contains("request_user_review"));
    assert!(schema.contains("use_current_plan"));
}

fn update_plan_tool() -> ToolSpec {
    plan_tool(UPDATE_PLAN_TOOL_NAME)
}

fn plan_tool(name: &str) -> ToolSpec {
    coordinator_plan_registered_tools()
        .expect("plan tools build")
        .into_iter()
        .find(|tool| tool.spec().name().as_str() == name)
        .unwrap_or_else(|| panic!("{name} is registered"))
        .spec()
        .clone()
}

fn valid_update_arguments() -> Value {
    serde_json::to_value(UpdatePlanInput {
        reason: "define a recursive plan".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(2),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Implement the requested change".to_owned(),
                acceptance: vec!["Focused tests pass".to_owned()],
                executor_policy: PlanExecutorPolicy::Auto,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    })
    .expect("valid update input serializes")
}
