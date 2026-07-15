use crate::plan::{
    PlanChangeInput, PlanExecutionIntent, PlanNodeInput, UpdatePlanInput,
    tools::{
        READ_PLAN_TOOL_NAME, UPDATE_PLAN_TOOL_NAME, coordinator_plan_registered_tools,
        scoped_child_plan_registered_tools, unbound_child_plan_registered_tools,
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
    assert_eq!(names, [READ_PLAN_TOOL_NAME, UPDATE_PLAN_TOOL_NAME]);
    assert!(tools.iter().all(|tool| {
        tool.action_kind() == crate::ToolActionKind::RuntimeControl
            && tool.spec().input_schema().as_schema().as_object().is_some()
    }));
}

#[test]
fn linked_child_plan_tools_are_scoped_and_unbound_children_have_none() {
    let tools = scoped_child_plan_registered_tools().expect("scoped child plan tools build");
    let names = tools
        .iter()
        .map(|tool| tool.spec().name().as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, [READ_PLAN_TOOL_NAME, UPDATE_PLAN_TOOL_NAME]);
    assert!(
        tools
            .iter()
            .all(|tool| tool.action_kind() == crate::ToolActionKind::RuntimeControl)
    );
    assert!(
        tools[0]
            .spec()
            .description()
            .contains("subtree below the linked task")
    );
    assert!(
        tools[1]
            .spec()
            .description()
            .contains("subtree below the linked task")
    );

    let read_schema = serde_json::to_string(tools[0].spec().input_schema())
        .expect("scoped read schema serializes");
    let update_schema = serde_json::to_string(tools[1].spec().input_schema())
        .expect("scoped update schema serializes");
    assert!(update_schema.contains("define_children"));
    assert!(!update_schema.contains("execution_intent"));
    assert!(read_schema.contains("max_depth"));

    assert!(
        unbound_child_plan_registered_tools()
            .expect("unbound child plan tools build")
            .is_empty()
    );
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
fn update_plan_schema_keeps_execution_policy_runtime_owned() {
    let schema = serde_json::to_value(update_plan_tool().input_schema())
        .expect("update_plan schema serializes");
    for field in ["executor_policy", "harness", "recovery_policy"] {
        assert!(
            !schema.to_string().contains(&format!("\"{field}\"")),
            "{field} must not be provider-authored"
        );
    }
}

#[test]
fn update_plan_schema_accepts_workspace_root_and_rejects_parent_traversal() {
    let tool = update_plan_tool();
    let validator = jsonschema::validator_for(tool.input_schema().as_schema().as_value())
        .expect("update_plan schema compiles");
    assert!(validator.is_valid(&valid_update_arguments()));
    assert!(validator.is_valid(&valid_update_arguments()));
    assert!(
        !validator.is_valid(&json!({
            "reason": "attempt to author runtime policy",
            "execution_intent": "continue_planning",
            "coordinator_node_id": null,
            "max_concurrency_hint": null,
            "change": {
                "type": "define_plan",
                "expected_plan_revision": 0,
                "root": {
                    "client_key": "root",
                    "objective": "bad",
                    "acceptance": [],
                    "depends_on": [],
                    "children": [],
                    "harness": {"write_scope": ["."]}
                }
            }
        })),
        "runtime-owned harness must not be provider-authored"
    );
}

#[test]
fn read_plan_schema_does_not_expose_legacy_attempt_selection() {
    let tool = plan_tool(READ_PLAN_TOOL_NAME);
    let validator = jsonschema::validator_for(tool.input_schema().as_schema().as_value())
        .expect("read_plan schema compiles");

    assert!(validator.is_valid(&json!({"max_depth": 4})));
    assert!(!validator.is_valid(&json!({"include_attempts": true})));
    assert!(tool.description().contains("Historical attempt"));
}

#[test]
fn attempt_reporting_and_control_tools_are_not_provider_visible() {
    let names = coordinator_plan_registered_tools()
        .expect("plan tools build")
        .into_iter()
        .map(|tool| tool.spec().name().as_str().to_owned())
        .collect::<Vec<_>>();
    for name in [
        "begin_plan",
        "control_plan_attempt",
        "report_plan_progress",
        "report_plan_attempt",
    ] {
        assert!(
            !names.iter().any(|visible| visible == name),
            "{name} leaked into the provider schema"
        );
    }
}

#[test]
fn update_plan_contract_explains_lifecycle_and_runtime_owned_state() {
    let tool = update_plan_tool();
    let description = tool.description();
    assert!(!description.contains("begin_plan"));
    assert!(description.contains("client_key"));
    assert!(description.contains("runtime-owned"));
    assert!(description.contains("execute_if_authorized"));
    assert!(description.contains("actual activity"));
    assert!(description.contains("already executing"));
    assert!(description.contains("use_current_plan"));

    let schema = serde_json::to_string(tool.input_schema()).expect("schema serializes");
    assert!(schema.contains("first valid update creates the Plan"));
    assert!(schema.contains("New nodes use client_key"));
    assert!(!schema.contains("crates/merry-runtime"));
    assert!(schema.contains("already requested execution"));
    assert!(schema.contains("execute_if_authorized"));
    assert!(schema.contains("request_user_review"));
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
                status: None,
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
