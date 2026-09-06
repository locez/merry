use crate::{
    coding::{
        CodingSubagentsConfig, HeadlessCodingRuntimeInput, ProcessExecutionMode,
        build_headless_coding, coding_agent_process_admission, fixed_process_backend,
    },
    runtime_events::collect_runtime_step_events,
    testing::{FakeProcessRunner, ScriptedProvider, model_name},
};
use merry::profiles::CODING_LOOP_PROCESS_TOOL;
use merry_core::{ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{FinishReason, ModelEvent, ModelOutput, ModelResponse};
use merry_process::ProcessSession;
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, ProcessRunner, RegisteredTool, StepContext, StepInput,
    ToolExecutionContext, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn unrestricted_mode_admits_the_host_process_profile() {
    let admission = coding_agent_process_admission(None, ProcessExecutionMode::Unrestricted)
        .await
        .expect("unrestricted mode should admit the host process profile");

    assert_eq!(
        admission.sandbox_profile(),
        merry_runtime::LocalWorkspaceProcessSandboxProfile::Host
    );
}

#[tokio::test(flavor = "current_thread")]
async fn inner_only_mode_admits_the_inner_local_workspace_process_profile() {
    let admission = coding_agent_process_admission(None, ProcessExecutionMode::InnerOnly)
        .await
        .expect("inner-only mode should admit the inner process profile");

    assert_eq!(
        admission.sandbox_profile(),
        merry_runtime::LocalWorkspaceProcessSandboxProfile::LocalWorkspace
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

    let runtime = build_headless_coding(HeadlessCodingRuntimeInput {
        session_id: "headless-coding-runtime-profile",
        root: &workspace,
        provider: Arc::new(provider.clone()),
        model: model_name(),
        process_backend: fixed_process_backend(ProcessSession::from_parts(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
            permissioned_factory,
        )),
        extra_tools: Vec::new(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
        workspace_tool_limits: None,
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
    assert!(request_text.contains("Coding file capabilities"));
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

    let runtime = build_headless_coding(HeadlessCodingRuntimeInput {
        session_id: "headless-coding-runtime-extra-tools",
        root: &workspace,
        provider: Arc::new(provider.clone()),
        model: model_name(),
        process_backend: fixed_process_backend(ProcessSession::from_parts(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
            permissioned_factory,
        )),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
        workspace_tool_limits: None,
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
