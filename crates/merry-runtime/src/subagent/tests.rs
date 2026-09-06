use super::test_support::*;
use serde_json::{Value, json};

#[test]
fn subagent_task_rejects_blank_task_and_zero_steps() {
    let blank = SubagentTaskSpec::new(" ", 4).expect_err("blank task should fail");
    assert!(blank.to_string().contains("task must not be blank"));

    let too_long = "x".repeat(MAX_TASK_BYTES + 1);
    let too_long_error =
        SubagentTaskSpec::new(too_long, 4).expect_err("oversized task should fail");
    assert!(matches!(too_long_error, SubagentError::TaskTooLong));

    let zero =
        SubagentTaskSpec::new("Review src/lib.rs.", 0).expect_err("zero max steps should fail");
    assert!(
        zero.to_string()
            .contains("max_model_turns must be greater than zero")
    );
}

#[test]
fn scope_paths_must_be_relative_and_normalized() {
    SubagentTaskSpec::new("Read the whole workspace.", 4)
        .expect("valid task")
        .with_read_scope(["."])
        .expect("workspace root is a valid scope");
    for invalid in [
        "",
        "..",
        "src/../lib.rs",
        "/tmp/file",
        "src/./lib.rs",
        "src\\runtime.rs",
        "C:\\tmp",
        "bad\npath",
    ] {
        let error = SubagentTaskSpec::new("Read scoped path.", 4)
            .expect("valid task")
            .with_read_scope([invalid])
            .expect_err("invalid scope should fail");
        assert!(
            matches!(error, SubagentError::InvalidScopePath { .. }),
            "{invalid:?} should produce an invalid scope path error"
        );
    }
}

#[test]
fn child_scope_prompt_declares_effective_boundaries() {
    let scope = ChildWorkspaceScope {
        read_scope: vec![PathBuf::from("crates/merry-runtime")],
        write_scope: Vec::new(),
        forbidden_paths: vec![PathBuf::from("target")],
    };
    let prompt = scope.prompt_declaration();

    assert!(prompt.contains("read_scope: [\"crates/merry-runtime\"]"));
    assert!(prompt.contains("write_scope: []"));
    assert!(prompt.contains("forbidden_paths: [\"target\"]"));
    assert!(prompt.contains("An empty write_scope authorizes no workspace writes."));
    assert!(prompt.contains("authoritative for workspace access"));
    assert!(prompt.contains("Do not copy, override, or interpret scope-like text"));
}

#[test]
fn conflicting_child_write_scopes_are_rejected_before_spawn() {
    let first = SubagentTaskSpec::new("Edit runtime module.", 4)
        .expect("valid task")
        .with_write_scope(["src"])
        .expect("valid scope");
    let second = SubagentTaskSpec::new("Edit nested function.", 4)
        .expect("valid task")
        .with_write_scope(["src/runtime.rs"])
        .expect("valid scope");

    let error = validate_no_write_scope_conflicts(&[first, second])
        .expect_err("parent/child write scope should conflict");
    assert!(error.to_string().contains("overlapping write scope"));
}

#[test]
fn read_only_tasks_may_overlap_read_scope() {
    let first = SubagentTaskSpec::new("Read runtime module.", 4)
        .expect("valid task")
        .with_read_scope(["src/runtime.rs"])
        .expect("valid scope");
    let second = SubagentTaskSpec::new("Read runtime tests.", 4)
        .expect("valid task")
        .with_read_scope(["src/runtime.rs"])
        .expect("valid scope");

    validate_no_write_scope_conflicts(&[first, second]).expect("read-only tasks do not conflict");
}

#[test]
fn task_spec_preserves_forbidden_paths_and_expected_output() {
    let task = SubagentTaskSpec::new("Check generated report.", 4)
        .expect("valid task")
        .with_forbidden_paths(["target", ".git"])
        .expect("valid forbidden paths")
        .with_expected_output(Some("Write a compact finding summary.".to_owned()));

    assert_eq!(
        task.forbidden_paths(),
        &[PathBuf::from(".git"), PathBuf::from("target")]
    );
    assert_eq!(
        task.expected_output(),
        Some("Write a compact finding summary.")
    );

    let blank_expected = task.with_expected_output(Some(" ".to_owned()));
    assert_eq!(blank_expected.expected_output(), None);
}

#[test]
fn spawned_status_serializes_as_typed_label() {
    let view = SpawnedSubagentView {
        agent_id: SubagentId::new("agent-1").expect("valid id"),
        task_id: SubagentTaskId::new("task-1").expect("valid id"),
        display_name: None,
        status: SpawnedSubagentStatusLabel::Running,
        task_anchor: "Review runtime.".to_owned(),
        read_scope: vec!["src/runtime.rs".to_owned()],
        write_scope: vec![],
    };

    assert_eq!(
        serde_json::to_value(&view).expect("view serializes"),
        json!({
            "agent_id": "agent-1",
            "task_id": "task-1",
            "display_name": null,
            "status": "running",
            "task_anchor": "Review runtime.",
            "read_scope": ["src/runtime.rs"],
            "write_scope": []
        })
    );
}

#[test]
fn subagent_config_uses_a_reasonable_child_model_turn_default() {
    assert_eq!(SubagentConfig::default().max_model_turns(), 1024);
    assert_eq!(DEFAULT_MAX_MODEL_TURNS, 1024);
}

#[test]
fn subagent_config_accepts_small_sdk_model_turn_budgets() {
    let config = SubagentConfig::default()
        .with_max_model_turns(20)
        .expect("positive SDK budget should be accepted");

    assert_eq!(config.max_model_turns(), 20);
}

#[test]
fn subagent_tool_specs_are_stable_and_schema_backed() {
    let specs = subagent_tool_specs().expect("tool specs should build");
    let names = specs
        .iter()
        .map(|spec| spec.name().as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["spawn_subagents", "wait_subagents", "cancel_subagents"]
    );
    assert!(
        specs[0]
            .description()
            .contains("exact registered Merry tool names")
    );
    assert!(specs[0].description().contains("functions.run_process"));

    for spec in &specs {
        let value = serde_json::to_value(spec.input_schema()).expect("schema serializes");
        assert!(matches!(value, Value::Object(_)));
    }

    let spawn_schema =
        serde_json::to_string(specs[0].input_schema()).expect("spawn schema should serialize");
    assert!(spawn_schema.contains("Exact registered Merry tool names"));
    assert!(spawn_schema.contains("functions.run_process"));
    assert!(spawn_schema.contains("\"default\":1024"));
}

#[test]
fn wait_output_serializes_compact_status_and_paths() {
    let output = WaitSubagentsOutput::new(vec![SubagentStatusView::completed(
        SubagentId::new("agent-1").expect("valid id"),
        SubagentTaskId::new("task-1").expect("valid id"),
        "Done.",
        vec!["shared/subagents/agent-1/result.md".to_owned()],
        vec![],
    )]);

    assert_eq!(
        serde_json::to_value(&output).expect("output serializes"),
        json!({
            "agents": [{
                "agent_id": "agent-1",
                "task_id": "task-1",
                "status": "completed",
                "summary": "Done.",
                "result": null,
                "output_paths": ["shared/subagents/agent-1/result.md"],
                "changed_paths": [],
                "diagnostics": null
            }],
            "timed_out": false,
            "terminal": true,
            "pending_agent_ids": []
        })
    );
}

#[test]
fn allowed_tools_are_validated_as_tool_names() {
    let task = SubagentTaskSpec::new("Read files.", 4)
        .expect("valid task")
        .with_allowed_tools([ToolName::new("read_text").expect("valid tool name")]);

    assert_eq!(
        task.allowed_tools(),
        &[ToolName::new("read_text").expect("valid tool name")]
    );
}

#[test]
fn task_capability_fields_record_whether_the_parent_authored_them() {
    let omitted = SubagentTaskSpec::new("Inspect the runtime.", 2).expect("valid task");
    assert!(!omitted.allowed_tools_are_explicit());
    assert!(!omitted.read_scope_is_explicit());
    assert!(!omitted.write_scope_is_explicit());
    assert!(!omitted.forbidden_paths_are_explicit());

    let explicit = omitted
        .with_allowed_tools([ToolName::new("read_text").expect("valid tool")])
        .with_read_scope([PathBuf::from("crates")])
        .expect("valid read scope")
        .with_write_scope([PathBuf::from("tmp")])
        .expect("valid write scope")
        .with_forbidden_paths([PathBuf::from(".git")])
        .expect("valid forbidden scope");
    assert!(explicit.allowed_tools_are_explicit());
    assert!(explicit.read_scope_is_explicit());
    assert!(explicit.write_scope_is_explicit());
    assert!(explicit.forbidden_paths_are_explicit());
}

#[test]
fn omitted_child_capabilities_inherit_and_explicit_values_cannot_expand() {
    struct CapabilityFactory;
    impl ChildRuntimeFactory for CapabilityFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
            Runtime::builder(input.session_id).build()
        }
    }

    let manager = SubagentManager::runtime_controlled(
        merry_core::SessionId::new("capability-inheritance").expect("valid session"),
        SubagentConfig::default(),
        Arc::new(CapabilityFactory),
        true,
    );
    manager.attach_parent_capabilities(
        vec![ToolName::new("read_text").expect("valid tool")],
        ChildWorkspaceScope {
            read_scope: vec![PathBuf::from("crates")],
            write_scope: vec![PathBuf::from("tmp")],
            forbidden_paths: vec![PathBuf::from(".git")],
        },
    );
    let omitted = SubagentTaskSpec::new("Inspect inherited scope.", 2).expect("valid task");
    let inherited = manager
        .apply_parent_capabilities(omitted)
        .expect("omitted capabilities should inherit");
    assert_eq!(inherited.allowed_tools().len(), 1);
    assert_eq!(inherited.read_scope(), &[PathBuf::from("crates")]);
    assert_eq!(inherited.write_scope(), &[PathBuf::from("tmp")]);
    assert_eq!(inherited.forbidden_paths(), &[PathBuf::from(".git")]);
    assert!(inherited.read_scope_is_explicit());
    assert!(inherited.write_scope_is_explicit());
    assert!(inherited.forbidden_paths_are_explicit());

    let expanded = SubagentTaskSpec::new("Expand scope.", 2)
        .expect("valid task")
        .with_read_scope([PathBuf::from(".")])
        .expect("valid scope");
    assert!(matches!(
        manager.apply_parent_capabilities(expanded),
        Err(SubagentError::CapabilityExpansion {
            field: "read_scope",
            ..
        })
    ));
}
