use super::{
    CodingLoopRuntimeOptions, CodingSubagentsConfig, HeadlessCodingRuntimeInput,
    build_coding_loop_runtime, build_headless_coding_runtime, coding_agent_process_admission,
    resume_headless_coding_runtime,
};

use crate::debug::coding_loop::coding_loop_workspace_call;
use crate::runtime_events::{collect_runtime_step_events, first_pending_tool_call};
use crate::testing::{FakeProcessRunner, ScriptedProvider, model_name};
use merry_core::{
    RuntimeJournalEvent, ToolCallResult, ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelProvider, ModelResponse, ModelToolCall,
    ModelToolCallId, ToolArguments,
};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AgentLoopStatus, FileSessionStore,
    PermissionedProcessRunnerFactory, ProcessRunner, RegisteredTool, StepContext, StepInput,
    SubagentConfig, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use merry_tool_workspace::{CODING_LOOP_PROCESS_TOOL, WORKSPACE_READ_FILE_TOOL};
use serde_json::{Map, Value};
use std::{path::Path, sync::Arc};

#[tokio::test(flavor = "current_thread")]
async fn no_outer_sandbox_still_admits_the_inner_bwrap_process_profile() {
    let admission = coding_agent_process_admission(None, true)
        .await
        .expect("explicit host mode should retain inner process admission");

    assert_eq!(
        admission.sandbox_profile(),
        merry_runtime::LocalWorkspaceProcessSandboxProfile::CliBwrapV1
    );
}

struct StaticOkExecutor;

impl ToolExecutor for StaticOkExecutor {
    fn execute<'a>(
        &'a self,
        _call: merry_core::PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async { Ok(ToolExecutionOutcome::succeeded_text("ok")) })
    }
}

fn headless_input<'a>(
    session_id: &'a str,
    root: &'a Path,
    provider: Arc<dyn ModelProvider>,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
) -> HeadlessCodingRuntimeInput<'a> {
    HeadlessCodingRuntimeInput {
        session_id,
        root,
        admission: AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        provider,
        model: model_name(),
        runner,
        permissioned_process_runner_factory,
        extra_tools: Vec::new(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn coding_runtime_projects_root_agents_in_the_stable_prefix() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::write(
        workspace.join("AGENTS.md"),
        "Use root project rule sentinel.\n",
    )
    .expect("write root project rules");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_loop_runtime(
        "coding-loop-root-project-rules",
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
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect project rules.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");

    let requests = provider.recorded_requests();
    let request = &requests[0];
    let stable_text = request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(request.stable_prefix_message_count(), 3);
    assert!(stable_text.contains("project-rules-source:AGENTS.md"));
    assert!(stable_text.contains("Use root project rule sentinel."));
}

#[tokio::test(flavor = "current_thread")]
async fn coding_runtime_omits_project_rules_when_root_agents_is_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runtime = build_coding_loop_runtime(
        "coding-loop-missing-root-project-rules",
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
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect project rules.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");

    let requests = provider.recorded_requests();
    let request = &requests[0];
    assert!(request.stable_prefix_messages().iter().all(|message| {
        !message
            .content()
            .as_text()
            .contains("project-rules-source:")
    }));
    assert!(request.messages().iter().all(|message| {
        !message
            .content()
            .as_text()
            .contains("AGENTS.md unavailable")
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn resumed_coding_runtime_reloads_current_root_agents() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let rules_path = workspace.join("AGENTS.md");
    std::fs::write(&rules_path, "Use saved project rule A.\n").expect("write rule A");
    let store = FileSessionStore::new(temp.path().join("sessions"));
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
    let permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory> = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );

    let first_provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("first done")],
            FinishReason::Stop,
            None,
        ),
    })]]);
    let first_runtime = build_headless_coding_runtime(headless_input(
        "headless-resume-reloads-project-rules",
        &workspace,
        Arc::new(first_provider.clone()),
        Arc::clone(&runner),
        Arc::clone(&permissioned_factory),
    ))
    .expect("first runtime should build");
    collect_runtime_step_events(
        &first_runtime,
        StepInput::user_text("Inspect rule A.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("first runtime step should complete");
    first_runtime
        .save_session_to(store.clone())
        .await
        .expect("first runtime should save");
    let first_requests = first_provider.recorded_requests();
    let first_stable_prefix_hash = first_requests[0].stable_prefix_hash().clone();

    std::fs::write(&rules_path, "Use current project rule B.\n").expect("write rule B");
    let resumed_provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("resumed done")],
            FinishReason::Stop,
            None,
        ),
    })]]);
    let resumed_runtime = resume_headless_coding_runtime(
        headless_input(
            "headless-resume-reloads-project-rules",
            &workspace,
            Arc::new(resumed_provider.clone()),
            runner,
            permissioned_factory,
        ),
        store,
    )
    .await
    .expect("runtime should resume");
    collect_runtime_step_events(
        &resumed_runtime,
        StepInput::user_text("Inspect current rules.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("resumed runtime step should complete");

    let resumed_requests = resumed_provider.recorded_requests();
    let resumed_request = &resumed_requests[0];
    let resumed_stable_text = resumed_request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(resumed_stable_text.contains("Use current project rule B."));
    assert!(!resumed_stable_text.contains("Use saved project rule A."));
    assert_ne!(
        resumed_request.stable_prefix_hash(),
        &first_stable_prefix_hash
    );
}

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
            extra_tools: Vec::new(),
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
    assert!(stable_text.contains("$skill-name"));
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
        extra_tools: Vec::new(),
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
async fn headless_runtime_registers_extra_tools() {
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
    let schema = schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "properties": {}
    }))
    .expect("test schema should be valid");
    let spec = ToolSpec::new(
        ToolName::new("mcp_docs_read").expect("valid tool name"),
        "Read docs through MCP",
        ToolInputSchema::new(schema).expect("schema should be valid"),
    )
    .expect("tool spec should be valid");

    let runtime = build_headless_coding_runtime(HeadlessCodingRuntimeInput {
        session_id: "headless-coding-runtime-extra-tools",
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
        extra_tools: vec![RegisteredTool::read_only(spec, Arc::new(StaticOkExecutor))],
    })
    .expect("headless coding runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect tools.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");

    let request = provider.recorded_requests()[0].clone();
    assert!(
        request
            .tools()
            .iter()
            .any(|tool| tool.name().as_str() == "mcp_docs_read")
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
            extra_tools: Vec::new(),
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
            extra_tools: Vec::new(),
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
            extra_tools: Vec::new(),
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
    std::fs::write(
        workspace.join("AGENTS.md"),
        "Child must receive root rule sentinel.\n",
    )
    .expect("write root project rules");

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
            extra_tools: Vec::new(),
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
    let child_stable_text = child_request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(child_stable_text.contains("project-rules-source:AGENTS.md"));
    assert!(child_stable_text.contains("Child must receive root rule sentinel."));
    assert!(child_tool_names.contains(&"workspace_read_file"));
    assert!(child_tool_names.contains(&"workspace_list_dir"));
    assert!(child_tool_names.contains(&"workspace_search_text"));
    assert!(child_tool_names.contains(&"run_process"));
    assert!(!child_tool_names.contains(&"workspace_patch"));
    assert!(!child_tool_names.contains(&"spawn_subagents"));
    assert!(!child_tool_names.contains(&"wait_subagents"));
    assert!(!child_tool_names.contains(&"cancel_subagents"));
}

fn resolved_tool_result(events: &[RuntimeJournalEvent]) -> &ToolCallResult {
    events
        .iter()
        .find_map(|event| match &event.payload {
            merry_core::RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("events should include a resolved tool result")
}
