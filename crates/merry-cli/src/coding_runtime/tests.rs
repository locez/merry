use super::{
    CodingLoopRuntimeOptions, CodingSubagentsConfig, HeadlessCodingRuntimeInput,
    build_coding_loop_runtime, build_headless_coding_runtime,
};
use crate::debug::coding_loop::coding_loop_workspace_call;
use crate::runtime_events::{collect_runtime_step_events, first_pending_tool_call};
use crate::testing::{FakeProcessRunner, ScriptedProvider, model_name};
use merry_core::{RuntimeEvent, ToolCallResult, ToolCallResultStatus, ToolName};
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments,
};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AgentLoopStatus, ProcessRunner,
    StepContext, StepInput, SubagentConfig, ToolExecutionContext,
};
use merry_tool_workspace::{CODING_LOOP_PROCESS_TOOL, WORKSPACE_READ_FILE_TOOL};
use serde_json::{Map, Value};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn projects_skill_metadata_without_body() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let skill_root = temp.path().join("skills");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::create_dir_all(skill_root.join("demo")).expect("mkdir skill");
    std::fs::write(
        skill_root.join("demo/SKILL.md"),
        "---\nname: demo-skill\ndescription: Use for demo tasks.\n---\n# Demo\nbody sentinel\n",
    )
    .expect("write skill");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_loop_runtime(
        "coding-loop-skill-prefix",
        &workspace,
        AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        Arc::new(provider.clone()),
        model_name(),
        Arc::new(FakeProcessRunner::succeeding("")),
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            permissioned_process_runner_factory: None,
            skill_roots: vec![skill_root.clone()],
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect skills.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");
    let request = provider.recorded_requests()[0].clone();
    let stable_text = request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    let request_text = request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(stable_text.contains("demo-skill"));
    assert!(stable_text.contains("Use for demo tasks."));
    assert!(stable_text.contains("demo/SKILL.md"));
    assert!(request_text.contains("workspace_read_file"));
    assert!(request_text.contains("Workspace coding profile"));
    assert!(request_text.contains("user's current input language"));
    assert!(request_text.contains("configured sandbox/profile"));
    assert!(request_text.contains("network access may be intentionally restricted"));
    assert!(request_text.contains("call request_permissions for that exact action"));
    assert!(!stable_text.contains("body sentinel"));
}

#[tokio::test(flavor = "current_thread")]
async fn headless_runtime_uses_coding_agent_profile() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
    let permissioned_factory = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );

    let runtime = build_headless_coding_runtime(HeadlessCodingRuntimeInput {
        session_id: "headless-coding-runtime-profile",
        root: &workspace,
        admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        provider: Arc::new(provider.clone()),
        model: model_name(),
        runner,
        permissioned_process_runner_factory: permissioned_factory,
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
    })
    .expect("headless coding runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect workspace.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");

    let request = provider.recorded_requests()[0].clone();
    let request_text = request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(request_text.contains("Workspace coding profile"));
    assert!(request_text.contains("user's current input language"));
    assert!(
        request
            .tools()
            .iter()
            .any(|tool| tool.name().as_str() == CODING_LOOP_PROCESS_TOOL)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn includes_skill_roots_in_workspace_read_tools() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let skill_root = temp.path().join("skills");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::create_dir_all(skill_root.join("demo")).expect("mkdir skill");
    std::fs::write(
        skill_root.join("demo/SKILL.md"),
        "---\nname: demo-skill\ndescription: Demo skill.\n---\n# Demo\n",
    )
    .expect("write skill");

    let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_workspace_call(
        "call-read-skill",
        WORKSPACE_READ_FILE_TOOL,
        [(
            "path",
            serde_json::Value::String("demo/SKILL.md".to_owned()),
        )],
    )
    .expect("workspace read call should build"))]]);
    let runtime = build_coding_loop_runtime(
        "coding-loop-skill-root-read",
        &workspace,
        AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        Arc::new(provider.clone()),
        model_name(),
        Arc::new(FakeProcessRunner::succeeding("")),
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            permissioned_process_runner_factory: None,
            skill_roots: vec![skill_root.clone()],
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    let events = collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Read demo skill.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should collect pending skill read");
    let pending = first_pending_tool_call(&events).expect("pending skill read");
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("skill read should execute");
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
}

#[tokio::test(flavor = "current_thread")]
async fn allows_missing_default_skill_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let missing_skill_root = temp.path().join("config/merry/skills");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_loop_runtime(
        "coding-loop-missing-default-skill-root",
        &workspace,
        AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        Arc::new(provider.clone()),
        model_name(),
        Arc::new(FakeProcessRunner::succeeding("")),
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            permissioned_process_runner_factory: None,
            skill_roots: vec![missing_skill_root],
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("missing default skill root should not block runtime");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Run without configured skills.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");

    let request = provider.recorded_requests()[0].clone();
    assert!(
        request
            .stable_prefix_messages()
            .iter()
            .all(|message| !message.content().as_text().contains("## Skills"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn hides_subagent_tools_by_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_loop_runtime(
        "coding-loop-subagents-default-off",
        &workspace,
        AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        Arc::new(provider.clone()),
        model_name(),
        Arc::new(FakeProcessRunner::succeeding("")),
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            permissioned_process_runner_factory: None,
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
    let runtime = build_coding_loop_runtime(
        "coding-loop-subagents-enabled",
        &workspace,
        AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        Arc::new(provider.clone()),
        model_name(),
        Arc::new(FakeProcessRunner::succeeding("")),
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            permissioned_process_runner_factory: None,
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::enabled(
                SubagentConfig::new(2, 1).expect("valid subagent config"),
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
async fn subagent_with_narrow_tools_keeps_read_only_profile() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::write(workspace.join("README.md"), "child fixture\n").expect("write fixture");

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
                                Value::Number(serde_json::Number::from(1)),
                            ),
                            (
                                "allowed_tools".to_owned(),
                                Value::Array(vec![Value::String("workspace_read_file".to_owned())]),
                            ),
                        ]))]),
                    )])))
                    .expect("valid spawn args"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("child done")],
                FinishReason::Stop,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("parent done")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let runtime = build_coding_loop_runtime(
        "coding-loop-subagent-narrow-tools",
        &workspace,
        AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        Arc::new(provider.clone()),
        model_name(),
        Arc::new(FakeProcessRunner::succeeding("")),
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            retry_policy: None,
            context_compaction: None,
            permissioned_process_runner_factory: None,
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::enabled(
                SubagentConfig::new(2, 1).expect("valid subagent config"),
            ),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

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
    assert_eq!(requests.len(), 3);
    let child_request = requests
        .iter()
        .find(|request| {
            request
                .dynamic_messages()
                .iter()
                .any(|message| message.content().as_text().contains("Inspect the fixture."))
        })
        .expect("child request should be recorded");
    let child_tool_names = child_request
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();
    assert!(child_tool_names.contains(&"workspace_read_file"));
    assert!(child_tool_names.contains(&"workspace_list_dir"));
    assert!(child_tool_names.contains(&"workspace_search_text"));
    assert!(child_tool_names.contains(&"run_process"));
    assert!(!child_tool_names.contains(&"workspace_patch"));
    assert!(!child_tool_names.contains(&"spawn_subagents"));
    assert!(!child_tool_names.contains(&"wait_subagents"));
    assert!(!child_tool_names.contains(&"cancel_subagents"));
}

fn resolved_tool_result(events: &[RuntimeEvent]) -> &ToolCallResult {
    events
        .iter()
        .find_map(|event| match &event.kind {
            merry_core::RuntimeEventKind::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("events should include a resolved tool result")
}
