use crate::{
    coding::{
        CodingRuntimeOptions, CodingSubagentsConfig, build_coding_runtime,
        tests::test_process_backend,
    },
    runtime_events::collect_runtime_step_events,
    testing::{ScriptedProvider, model_name},
};
use merry_core::ToolName;
use merry_llm::{
    FinishReason, ModelEvent, ModelMessageRole, ModelOutput, ModelResponse, ModelToolCall,
    ModelToolCallId, ToolArguments,
};
use merry_runtime::{AgentLoopConfig, AgentLoopStatus, StepContext, StepInput, SubagentConfig};
use serde_json::{Map, Value};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn hides_subagent_tools_by_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_runtime(
        "coding-loop-subagents-default-off",
        &workspace,
        Arc::new(provider.clone()),
        model_name(),
        CodingRuntimeOptions {
            approval_review: None,
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect available tools.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");
    let requests = provider.recorded_requests();
    let tool_names = requests[0]
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();

    assert!(!tool_names.contains(&"spawn_subagents"));
    assert!(!tool_names.contains(&"wait_subagents"));
    assert!(!tool_names.contains(&"cancel_subagents"));
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_subagent_tools_when_enabled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_runtime(
        "coding-loop-subagents-enabled",
        &workspace,
        Arc::new(provider.clone()),
        model_name(),
        CodingRuntimeOptions {
            approval_review: None,
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::enabled(
                SubagentConfig::new(2, 1)
                    .expect("valid subagent config")
                    .with_model_turn_bounds(2048, 2048)
                    .expect("coding subagent bounds should be valid"),
            ),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect available tools.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");
    let requests = provider.recorded_requests();
    let tool_names = requests[0]
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();

    assert!(tool_names.contains(&"spawn_subagents"));
    assert!(tool_names.contains(&"wait_subagents"));
    assert!(tool_names.contains(&"cancel_subagents"));
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_with_narrow_tools_keeps_stable_profile_and_runtime_admission() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::write(workspace.join("README.md"), "child fixture\n").expect("write fixture");
    std::fs::write(
        workspace.join("AGENTS.md"),
        "Child must receive root rule sentinel.\n",
    )
    .expect("write root project rules");

    let completion_step = || {
        vec![Ok::<_, merry_llm::ModelError>(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]
    };
    let provider = ScriptedProvider::new(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-spawn").expect("valid call id"),
                    ToolName::new("spawn_subagents").expect("valid tool name"),
                    ToolArguments::try_from(Value::Object(Map::from_iter([(
                        "tasks".to_owned(),
                        Value::Array(vec![Value::Object(Map::from_iter([
                            (
                                "task".to_owned(),
                                Value::String("Inspect the fixture.".to_owned()),
                            ),
                            (
                                "max_model_turns".to_owned(),
                                Value::Number(serde_json::Number::from(2048)),
                            ),
                            (
                                "allowed_tools".to_owned(),
                                Value::Array(vec![Value::String("read_text".to_owned())]),
                            ),
                            ("write_scope".to_owned(), Value::Array(Vec::new())),
                        ]))]),
                    )])))
                    .expect("valid spawn args"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        completion_step(),
        completion_step(),
        completion_step(),
        completion_step(),
        completion_step(),
        completion_step(),
        completion_step(),
    ]);
    let runtime = build_coding_runtime(
        "coding-loop-subagent-narrow-tools",
        &workspace,
        Arc::new(provider.clone()),
        model_name(),
        CodingRuntimeOptions {
            approval_review: None,
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::enabled(
                SubagentConfig::new(2, 1)
                    .expect("valid subagent config")
                    .with_model_turn_bounds(2048, 2048)
                    .expect("coding subagent bounds should be valid"),
            ),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    std::fs::write(
        workspace.join("AGENTS.md"),
        "Child must not reread changed root rule sentinel.\n",
    )
    .expect("replace root project rules after parent build");

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Delegate fixture inspection.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(3).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    let requests = provider.recorded_requests();
    assert!(
        requests.len() >= 2,
        "parent and child requests should be recorded"
    );
    let parent_system_text = requests[0]
        .dynamic_messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::System)
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(parent_system_text.contains("read_scope: [\".\"]"));
    assert!(parent_system_text.contains("write_scope: [\".\"]"));
    assert!(parent_system_text.contains("forbidden_paths: []"));
    let child_request = requests
        .iter()
        .find(|request| {
            request
                .dynamic_messages()
                .iter()
                .any(|message| message.content().as_text().contains("Inspect the fixture."))
        })
        .expect("child request should be recorded");
    let child_system_text = child_request
        .dynamic_messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::System)
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(child_system_text.contains("<merry_subagent_workspace_scope>"));
    assert!(child_system_text.contains("read_scope: [\".\"]"));
    assert!(child_system_text.contains("write_scope: []"));
    assert!(child_system_text.contains("forbidden_paths: []"));
    let child_user_text = child_request
        .dynamic_messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::User)
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(child_user_text, "Inspect the fixture.");
    assert!(!child_user_text.contains("read_scope"));
    let child_tool_names = child_request
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();
    let child_stable_text = child_request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(child_stable_text.contains("project-rules-source:AGENTS.md"));
    assert!(child_stable_text.contains("Child must receive root rule sentinel."));
    assert!(!child_stable_text.contains("Child must not reread changed root rule sentinel."));
    assert!(child_tool_names.contains(&"read_text"));
    assert_eq!(
        child_tool_names,
        [
            "run_process",
            "request_permissions",
            "read_text",
            "apply_patch",
            "web_fetch",
            "browser_tab",
            "browser_network",
            "spawn_subagents",
            "send_subagent_message",
            "wait_subagents",
            "cancel_subagents",
        ]
    );
    assert!(
        child_request
            .tool_profile_hash()
            .as_str()
            .starts_with("fnv1a64:")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn custom_browser_role_routes_subagents_to_role_provider_and_model() {
    use merry::profiles::{SubagentModelOverride, SubagentRoleCatalog, SubagentRoleOverlay};
    use merry_llm::ModelName;

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let completion_step = || {
        vec![Ok::<_, merry_llm::ModelError>(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]
    };
    let primary = ScriptedProvider::new(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-spawn-browser").expect("valid call id"),
                    ToolName::new("spawn_subagents").expect("valid tool name"),
                    ToolArguments::try_from(Value::Object(Map::from_iter([(
                        "tasks".to_owned(),
                        Value::Array(vec![Value::Object(Map::from_iter([
                            (
                                "task".to_owned(),
                                Value::String("Verify the login form UI.".to_owned()),
                            ),
                            ("role".to_owned(), Value::String("browser".to_owned())),
                        ]))]),
                    )])))
                    .expect("valid spawn args"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        completion_step(),
        completion_step(),
        completion_step(),
        completion_step(),
    ]);
    let browser_provider = ScriptedProvider::new(vec![
        completion_step(),
        completion_step(),
        completion_step(),
    ]);

    let mut catalog = SubagentRoleCatalog::builtin();
    catalog
        .apply_role_overlay(
            "browser",
            SubagentRoleOverlay {
                base: Some("review".to_owned()),
                description: Some("UI verification specialist".to_owned()),
                model_override: Some(SubagentModelOverride {
                    provider: Arc::new(browser_provider.clone()),
                    model: ModelName::new("browser-vision-test").expect("valid browser model"),
                    retry_policy: Some(merry_llm::ModelRetryPolicy::disabled()),
                }),
                allowed_tools: Some(vec![
                    ToolName::new("browser_tab").expect("valid tool"),
                    ToolName::new("browser_network").expect("valid tool"),
                    ToolName::new("read_text").expect("valid tool"),
                ]),
                ..SubagentRoleOverlay::default()
            },
        )
        .expect("custom browser role should apply");

    let runtime = build_coding_runtime(
        "coding-loop-browser-role-routing",
        &workspace,
        Arc::new(primary.clone()),
        model_name(),
        CodingRuntimeOptions {
            approval_review: None,
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            retry_policy: Some(merry_llm::ModelRetryPolicy::disabled()),
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::enabled(
                SubagentConfig::new(2, 1)
                    .expect("valid subagent config")
                    .with_model_turn_bounds(1, 80)
                    .expect("coding subagent bounds should be valid"),
            )
            .with_role_catalog(catalog),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Verify UI.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(3).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    // Parent tool description must advertise the custom browser role.
    let parent_spawn_desc = primary.recorded_requests()[0]
        .tools()
        .iter()
        .find(|tool| tool.name().as_str() == "spawn_subagents")
        .expect("spawn_subagents tool present")
        .description()
        .to_owned();
    assert!(
        parent_spawn_desc.contains("browser"),
        "expected custom browser role advertised in spawn_subagents description: {parent_spawn_desc}"
    );

    let browser_requests = browser_provider.recorded_requests();
    assert!(!browser_requests.is_empty());
    assert_eq!(browser_requests[0].model().as_str(), "browser-vision-test");
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_primary_model_switch_routes_subsequent_subagents_to_new_model() {
    use merry_core::{InteractiveRunState, RuntimeEvent};
    use merry_llm::{ModelName, ModelRetryPolicy};
    use merry_runtime::{InteractivePrimaryModel, InteractiveSettingsUpdate};

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let completion_step = || {
        vec![Ok::<_, merry_llm::ModelError>(ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
        })]
    };
    let initial_provider = ScriptedProvider::new(vec![completion_step()]);
    let switched_provider = ScriptedProvider::new(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-spawn-switched").expect("valid call id"),
                    ToolName::new("spawn_subagents").expect("valid tool name"),
                    ToolArguments::try_from(Value::Object(Map::from_iter([(
                        "tasks".to_owned(),
                        Value::Array(vec![Value::Object(Map::from_iter([
                            (
                                "task".to_owned(),
                                Value::String("Inspect workspace after model switch.".to_owned()),
                            ),
                            ("role".to_owned(), Value::String("explore".to_owned())),
                            ("wait".to_owned(), Value::String("all".to_owned())),
                        ]))]),
                    )])))
                    .expect("valid spawn args"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        completion_step(),
        completion_step(),
        completion_step(),
    ]);

    let runtime = build_coding_runtime(
        "coding-loop-live-model-switch",
        &workspace,
        Arc::new(initial_provider.clone()),
        ModelName::new("initial-model").expect("valid model"),
        CodingRuntimeOptions {
            approval_review: None,
            automatic_compaction: merry_runtime::CompactionConfig::disabled(),
            retry_policy: Some(ModelRetryPolicy::disabled()),
            context_compaction: None,
            process_backend: test_process_backend(),
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::runtime_controlled(
                true,
                SubagentConfig::new(2, 1)
                    .expect("valid subagent config")
                    .with_model_turn_bounds(1, 80)
                    .expect("coding subagent bounds should be valid"),
            ),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::default(),
            AgentLoopConfig::new(4).expect("valid loop config"),
        )
        .expect("interactive run should start");
    let (mut stream, input, control) = run.split();

    control
        .update_settings(InteractiveSettingsUpdate::default().with_primary_model(
            InteractivePrimaryModel::new(
                Arc::new(switched_provider.clone()),
                ModelName::new("switched-model").expect("valid switched model"),
                ModelRetryPolicy::disabled(),
            ),
        ))
        .await
        .expect("settings update should apply");

    input
        .submit("Delegate after switching models.")
        .await
        .expect("user message should enqueue");

    let mut saw_active = false;
    while let Some(event) = stream
        .next_event()
        .await
        .expect("interactive stream event should succeed")
    {
        match event {
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            } if saw_active => break,
            RuntimeEvent::InteractiveRunStateChanged { state }
                if state != InteractiveRunState::WaitingForInput =>
            {
                saw_active = true;
            }
            _ => {}
        }
    }
    drop(input);
    control.close().await.expect("interactive run should close");
    while stream
        .next_event()
        .await
        .expect("interactive stream drain should succeed")
        .is_some()
    {}

    assert!(
        initial_provider.recorded_requests().is_empty(),
        "initial provider should not be called after switching primary model"
    );
    let switched_requests = switched_provider.recorded_requests();
    assert!(
        switched_requests.len() >= 2,
        "both parent and child should call the switched provider"
    );
    assert!(
        switched_requests
            .iter()
            .all(|req| req.model().as_str() == "switched-model"),
        "child must dynamically inherit switched-model from live parent runtime"
    );
}
