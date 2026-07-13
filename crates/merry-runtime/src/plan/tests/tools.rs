use crate::plan::tools::{COORDINATOR_PLAN_TOOL_NAMES, coordinator_plan_registered_tools};

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
