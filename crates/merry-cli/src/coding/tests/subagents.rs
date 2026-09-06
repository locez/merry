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
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
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
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
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
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
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
            "spawn_subagents",
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
