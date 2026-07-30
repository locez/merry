use crate::plan::{
    PlanChangeInput, PlanExecutionIntent, PlanNodeInput, UpdatePlanInput,
    projection::{
        CHILD_LINKED_SCOPE_GUIDANCE, CHILD_SCOPED_UPDATE_GUIDANCE,
        COORDINATOR_ACTIVE_LINK_MUTATION_GUIDANCE, COORDINATOR_LINKED_SUMMARIES_GUIDANCE,
        COORDINATOR_ROOT_SCOPE_GUIDANCE, LINKED_CHILD_DECOMPOSITION_GUIDANCE,
        RUNTIME_OWNED_EXECUTION_GUIDANCE,
    },
    tools::{
        READ_PLAN_TOOL_NAME, UPDATE_PLAN_TOOL_NAME, coordinator_plan_registered_tools,
        scoped_child_plan_registered_tools, unbound_child_plan_registered_tools,
    },
};
use crate::schema_contract::assert_provider_input_schema_fields_have_descriptions;
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
fn scoped_child_schema_accepts_direct_children_but_rejects_nested_input() {
    let tool = scoped_child_plan_registered_tools()
        .expect("scoped child tools build")
        .into_iter()
        .find(|tool| tool.spec().name().as_str() == UPDATE_PLAN_TOOL_NAME)
        .expect("scoped update tool is registered");
    let validator = jsonschema::validator_for(tool.spec().input_schema().as_schema().as_value())
        .expect("scoped update schema compiles");
    let direct = json!({
        "reason": "decompose the linked task",
        "change": {
            "type": "define_children",
            "expected_plan_revision": 1,
            "children": [{
                "client_key": "child",
                "objective": "Complete one part",
                "acceptance": []
            }]
        }
    });
    assert!(
        validator.is_valid(&direct),
        "direct child input should validate: {:?}",
        validator.iter_errors(&direct).collect::<Vec<_>>()
    );

    let mut nested = direct.clone();
    nested["change"]["children"][0]["children"] = json!([{
        "client_key": "grandchild",
        "objective": "Complete a deeper part",
        "acceptance": []
    }]);
    assert!(!validator.is_valid(&nested));
}

#[test]
fn update_plan_schema_documents_identity_rules_for_runtime_validation() {
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
    assert!(validator.is_valid(&missing));

    let mut conflicting = valid_update_arguments();
    conflicting["change"]["root"]["id"] = json!("plan-node-1");
    assert!(validator.is_valid(&conflicting));

    let schema = serde_json::to_string(tool.input_schema()).expect("schema serializes");
    assert!(schema.contains("exactly one identity field"));
    assert!(schema.contains("Runtime validates"));
    assert!(!schema.contains("oneOf constraint only enforces"));
}

#[test]
fn update_plan_schema_is_shallow_for_authored_node_children() {
    let schema = serde_json::to_value(update_plan_tool().input_schema())
        .expect("update_plan schema serializes");
    let node_schema = find_schema_with_description(
        &schema,
        "One authored plan node with at most one level of direct children",
    )
    .expect("authored node schema is present");

    assert!(
        node_schema["oneOf"].is_null(),
        "node identity must not use oneOf"
    );
    let child_schema = node_schema
        .pointer("/properties/children/items")
        .expect("authored node exposes direct children")
        .clone();
    let child_schema = resolve_schema_ref(&schema, &child_schema);
    assert!(
        child_schema.pointer("/properties/children").is_none(),
        "direct child schema must be shallow"
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
    assert!(description.contains("bindable_plan_client_keys"));
    assert!(description.contains("spawn_subagents.tasks[].plan_client_key"));
    assert!(description.contains("intentionally omits runtime node ids"));
    assert!(description.contains("runtime-owned"));
    assert!(description.contains("execute_if_authorized"));
    assert!(description.contains("actual activity"));
    assert!(description.contains("already executing"));
    assert!(description.contains("use_current_plan"));
    assert!(description.contains("nested string field change.type"));
    assert!(description.contains("Do not put type on the outer update object"));

    let schema = serde_json::to_string(tool.input_schema()).expect("schema serializes");
    assert!(schema.contains("first valid update creates the Plan"));
    assert!(schema.contains("New nodes use client_key"));
    assert!(!schema.contains("crates/merry-runtime"));
    assert!(schema.contains("already requested execution"));
    assert!(schema.contains("execute_if_authorized"));
    assert!(schema.contains("request_user_review"));
}

#[test]
fn update_plan_schema_teaches_the_nested_change_discriminator() {
    let tool = update_plan_tool();
    let validator = jsonschema::validator_for(tool.input_schema().as_schema().as_value())
        .expect("update_plan schema compiles");
    assert!(validator.is_valid(&valid_update_arguments()));

    let mut missing_type = valid_update_arguments();
    missing_type["change"]
        .as_object_mut()
        .expect("change is an object")
        .remove("type");
    assert!(!validator.is_valid(&missing_type));

    let mut outer_type = valid_update_arguments();
    outer_type["type"] = json!("define_plan");
    outer_type["change"]
        .as_object_mut()
        .expect("change is an object")
        .remove("type");
    assert!(!validator.is_valid(&outer_type));

    let schema = serde_json::to_value(update_plan_tool().input_schema())
        .expect("update_plan schema serializes");
    let change_schema = &schema["properties"]["change"];
    let description = change_schema["description"]
        .as_str()
        .expect("change schema has a description");

    assert!(description.contains("inside the change object"));
    assert_eq!(change_schema["examples"][0]["type"], "define_plan");
    assert!(
        schema
            .to_string()
            .contains("One authored plan node with at most one level of direct children")
    );
}

#[test]
fn provider_visible_plan_schemas_describe_every_field_and_match_runtime_bounds() {
    for tool in coordinator_plan_registered_tools()
        .expect("plan tools build")
        .into_iter()
    {
        assert_provider_input_schema_fields_have_descriptions(tool.spec());
    }

    let read_schema = serde_json::to_value(plan_tool(READ_PLAN_TOOL_NAME).input_schema())
        .expect("read_plan schema serializes");
    assert_eq!(read_schema["properties"]["max_depth"]["minimum"], 0);
    assert_eq!(read_schema["properties"]["max_depth"]["maximum"], 16);

    let update_schema = serde_json::to_value(update_plan_tool().input_schema())
        .expect("update_plan schema serializes");
    assert_eq!(
        update_schema["properties"]["max_concurrency_hint"]["minimum"],
        1
    );
    assert_eq!(
        update_schema["properties"]["max_concurrency_hint"]["maximum"],
        6
    );
}

#[test]
fn plan_tool_descriptions_explain_one_level_coordinator_and_child_contract() {
    let coordinator_tools = coordinator_plan_registered_tools().expect("coordinator tools build");
    let coordinator_read = coordinator_tools[0].spec().description();
    let coordinator_update = coordinator_tools[1].spec().description();
    for description in [coordinator_read, coordinator_update] {
        for fragment in [
            COORDINATOR_ACTIVE_LINK_MUTATION_GUIDANCE,
            COORDINATOR_ROOT_SCOPE_GUIDANCE,
            LINKED_CHILD_DECOMPOSITION_GUIDANCE,
            COORDINATOR_LINKED_SUMMARIES_GUIDANCE,
        ] {
            assert!(
                description.contains(fragment),
                "coordinator tool description must contain the shared fragment {fragment:?}"
            );
        }
    }
    let child_tools = scoped_child_plan_registered_tools().expect("child tools build");
    for tool in child_tools {
        let description = tool.spec().description();
        for fragment in [
            CHILD_LINKED_SCOPE_GUIDANCE,
            CHILD_SCOPED_UPDATE_GUIDANCE,
            RUNTIME_OWNED_EXECUTION_GUIDANCE,
        ] {
            assert!(
                description.contains(fragment),
                "child tool description must contain the shared fragment {fragment:?}"
            );
        }
        if description.contains("Update authored children") {
            assert!(description.contains("nested string field change.type"));
        }
    }
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
        reason: "define a shallow plan".to_owned(),
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

fn find_schema_with_description<'a>(value: &'a Value, needle: &str) -> Option<&'a Value> {
    if value
        .get("description")
        .and_then(Value::as_str)
        .is_some_and(|description| description.contains(needle))
    {
        return Some(value);
    }
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_schema_with_description(value, needle)),
        Value::Object(values) => values
            .values()
            .find_map(|value| find_schema_with_description(value, needle)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn resolve_schema_ref<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    let reference = schema
        .get("$ref")
        .and_then(Value::as_str)
        .expect("schema reference is a JSON pointer");
    root.pointer(reference.strip_prefix('#').expect("local schema reference"))
        .expect("schema reference resolves")
}
