use super::{HeadlessRunPersistence, RunExitStatus, run_agent_loop_with_persistence};
use crate::cli_error::CliError;
use crate::coding::{
    ActionProcessBackend, CodingSubagentsConfig, HeadlessCodingRuntimeInput, build_headless_coding,
    fixed_process_backend, resume_headless_coding,
};
use crate::testing::{FakeProcessRunner, ScriptedProvider, model_name};
use merry::profiles::DEFAULT_CODING_AGENT_MAX_MODEL_TURNS;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, SessionId, ToolCallResult, ToolInputSchema,
    ToolName, ToolOutput, ToolSpec,
};
use merry_llm::{
    FinishReason, ModelError, ModelEvent, ModelOutput, ModelProvider, ModelResponse, ModelToolCall,
    ModelToolCallId, ToolArguments,
};
use merry_process::ProcessSession;
use merry_runtime::{
    AgentLoopConfig, ArtifactContent, FileSessionStore, ProcessRunner, RegisteredTool, Runtime,
    SessionTranscriptItem, StepContext, StepInput, ToolExecutionContext, ToolExecutionError,
    ToolExecutor, ToolExecutorFuture,
};
use std::{
    io,
    path::Path,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::io::AsyncWrite;

fn completing_provider(text: &str) -> ScriptedProvider {
    ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    })]])
}

fn incomplete_provider() -> ScriptedProvider {
    ScriptedProvider::new(vec![vec![Err(ModelError::Cancelled)]])
}

fn infrastructure_error_provider(tool_name: &str) -> ScriptedProvider {
    ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(ModelToolCall::new(
                ModelToolCallId::new("call-runtime-settlement-error").expect("valid call id"),
                ToolName::new(tool_name).expect("valid tool name"),
                ToolArguments::try_from(serde_json::json!({})).expect("valid tool arguments"),
            ))],
            FinishReason::ToolCalls,
            None,
        ),
    })]])
}

struct InfrastructureErrorExecutor;

impl ToolExecutor for InfrastructureErrorExecutor {
    fn execute<'a>(
        &'a self,
        _call: merry_core::PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async {
            Err(ToolExecutionError::infrastructure(
                "temporary executor outage",
            ))
        })
    }
}

fn infrastructure_error_tool(tool_name: &str) -> RegisteredTool {
    let schema = schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    }))
    .expect("test tool schema should be valid");
    let spec = ToolSpec::new(
        ToolName::new(tool_name).expect("valid tool name"),
        "Exercise runtime settlement failure",
        ToolInputSchema::new(schema).expect("valid tool input schema"),
    )
    .expect("valid tool spec");
    RegisteredTool::read_only(spec, Arc::new(InfrastructureErrorExecutor))
}

fn fake_process_backend() -> ActionProcessBackend {
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding(""));
    let permissioned_factory = Arc::new(
        merry_runtime::StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );
    fixed_process_backend(ProcessSession::from_parts(
        merry_runtime::AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
        runner,
        permissioned_factory,
    ))
}

fn headless_input<'a>(
    session_id: &'a str,
    workspace: &'a Path,
    provider: Arc<dyn ModelProvider>,
) -> HeadlessCodingRuntimeInput<'a> {
    HeadlessCodingRuntimeInput {
        session_id,
        root: workspace,
        provider,
        model: model_name(),
        process_backend: fake_process_backend(),
        extra_tools: Vec::new(),
        allow_hidden_workspace_paths: false,
        automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
        retry_policy: None,
        context_compaction: None,
        approval_review: None,
        skill_roots: Vec::new(),
        subagents: CodingSubagentsConfig::default(),
        workspace_tool_limits: None,
    }
}

fn build_runtime(
    session_id: &SessionId,
    workspace: &Path,
    provider: Arc<dyn ModelProvider>,
) -> Runtime {
    build_runtime_with_tools(session_id, workspace, provider, Vec::new())
}

fn build_runtime_with_tools(
    session_id: &SessionId,
    workspace: &Path,
    provider: Arc<dyn ModelProvider>,
    extra_tools: Vec<RegisteredTool>,
) -> Runtime {
    let mut input = headless_input(session_id.as_str(), workspace, provider);
    input.extra_tools = extra_tools;
    build_headless_coding(input).expect("runtime should build")
}

async fn resumed_transcript(
    session_id: &SessionId,
    workspace: &Path,
    store: FileSessionStore,
) -> Vec<SessionTranscriptItem> {
    let resumed = resume_headless_coding(
        headless_input(
            session_id.as_str(),
            workspace,
            Arc::new(completing_provider("unused resumed answer")),
        ),
        store,
    )
    .await
    .expect("persisted session should resume");
    resumed
        .session_transcript()
        .await
        .expect("resumed transcript should be available")
}

fn assert_transcript_contains_user_input(transcript: &[SessionTranscriptItem], expected: &str) {
    assert!(
        transcript.iter().any(|item| matches!(
            item,
            SessionTranscriptItem::UserMessage { text, .. } if text == expected
        )),
        "persisted transcript should contain {expected:?}: {transcript:?}"
    );
}

async fn resolve_pending_after_runtime_error(runtime: &Runtime) -> Result<(), CliError> {
    let pending = runtime.pending_tool_calls().await;
    let [call] = pending.as_slice() else {
        panic!("runtime error should retain one pending tool call: {pending:?}");
    };
    let artifact = ArtifactRef::new(
        ArtifactId::new("reviewer-runtime-recovery").expect("valid artifact id"),
        ArtifactKind::Text,
    );
    let diagnostic = ErrorInfo::new(
        "reviewer_runtime_recovery",
        "reviewer resolved a pending call after runtime settlement failed",
    )
    .expect("valid diagnostic");
    runtime
        .submit_tool_result(
            ToolCallResult::failed(call.id().clone(), artifact, diagnostic),
            ArtifactContent::text("executor outage was recorded for resume"),
        )
        .await
        .expect("reviewer settlement should make the session resume-safe");
    Ok(())
}

struct FailingWriter {
    kind: io::ErrorKind,
    message: &'static str,
}

impl FailingWriter {
    const fn new(kind: io::ErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }
}

impl AsyncWrite for FailingWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::new(self.kind, self.message)))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::new(self.kind, self.message)))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_settlement_error_is_persisted_and_partial_state_can_resume() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let store = FileSessionStore::new(temp.path().join("sessions"));
    let session_id = SessionId::new("runtime-settlement-error-save").expect("valid session id");
    let tool_name = "runtime_settlement_failure";
    let runtime = build_runtime_with_tools(
        &session_id,
        &workspace,
        Arc::new(infrastructure_error_provider(tool_name)),
        vec![infrastructure_error_tool(tool_name)],
    );
    let result = run_agent_loop_with_persistence(
        &runtime,
        StepInput::user_text("retain this partial task").expect("valid input"),
        FailingWriter::new(io::ErrorKind::Other, "injected runtime output failure"),
        async {
            resolve_pending_after_runtime_error(&runtime).await?;
            Err(CliError::Unexpected(
                "injected runtime reviewer failure".to_owned(),
            ))
        },
        HeadlessRunPersistence {
            loop_config: AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS)
                .expect("valid config"),
            context: StepContext::default(),
            events_jsonl: true,
            session_store: store.clone(),
            session_id: &session_id,
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(CliError::Unexpected(message))
            if message.contains("temporary executor outage")
                && !message.contains("output")
                && !message.contains("reviewer")
    ));

    drop(runtime);
    let transcript = resumed_transcript(&session_id, &workspace, store).await;
    assert_transcript_contains_user_input(&transcript, "retain this partial task");
    assert!(transcript.iter().any(|item| matches!(
        item,
        SessionTranscriptItem::ToolResult {
            output: Some(ToolOutput::Text { text }),
            ..
        } if text == "executor outage was recorded for resume"
    )));
}

#[tokio::test(flavor = "current_thread")]
async fn save_error_precedes_runtime_settlement_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let invalid_store_root = temp.path().join("runtime-error-store-is-a-file");
    std::fs::write(&invalid_store_root, "not a directory")
        .expect("invalid store fixture should be written");
    let session_id = SessionId::new("runtime-error-save-precedence").expect("valid session id");
    let tool_name = "runtime_settlement_save_failure";
    let runtime = build_runtime_with_tools(
        &session_id,
        &workspace,
        Arc::new(infrastructure_error_provider(tool_name)),
        vec![infrastructure_error_tool(tool_name)],
    );
    let mut output = Vec::new();

    let result = run_agent_loop_with_persistence(
        &runtime,
        StepInput::user_text("save failure must win").expect("valid input"),
        &mut output,
        resolve_pending_after_runtime_error(&runtime),
        HeadlessRunPersistence {
            loop_config: AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS)
                .expect("valid config"),
            context: StepContext::default(),
            events_jsonl: false,
            session_store: FileSessionStore::new(invalid_store_root),
            session_id: &session_id,
        },
    )
    .await;
    match result {
        Err(CliError::Unexpected(message)) => {
            assert!(message.contains("session runtime-error-save-precedence could not be saved"));
            assert!(message.contains("session store IO error"));
            assert!(!message.contains("temporary executor outage"));
        }
        other => panic!("save failure should precede runtime failure, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn completed_jsonl_broken_pipe_drains_and_persists_before_success_mapping() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let store = FileSessionStore::new(temp.path().join("sessions"));
    let session_id = SessionId::new("completed-broken-pipe-save").expect("valid session id");
    let runtime = build_runtime(
        &session_id,
        &workspace,
        Arc::new(completing_provider("completed answer")),
    );

    let result = run_agent_loop_with_persistence(
        &runtime,
        StepInput::user_text("completed task").expect("valid input"),
        FailingWriter::new(io::ErrorKind::BrokenPipe, "closed JSONL consumer"),
        std::future::ready(Ok(())),
        HeadlessRunPersistence {
            loop_config: AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS)
                .expect("valid config"),
            context: StepContext::default(),
            events_jsonl: true,
            session_store: store.clone(),
            session_id: &session_id,
        },
    )
    .await;
    assert!(matches!(result, Err(CliError::BrokenPipe)));

    drop(runtime);
    let transcript = resumed_transcript(&session_id, &workspace, store).await;
    assert_transcript_contains_user_input(&transcript, "completed task");
    assert!(transcript.iter().any(|item| matches!(
        item,
        SessionTranscriptItem::AssistantText { text } if text == "completed answer"
    )));
}

#[tokio::test(flavor = "current_thread")]
async fn incomplete_terminal_run_is_persisted_and_remains_incomplete() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let store = FileSessionStore::new(temp.path().join("sessions"));
    let session_id = SessionId::new("incomplete-terminal-save").expect("valid session id");
    let runtime = build_runtime(&session_id, &workspace, Arc::new(incomplete_provider()));
    let mut output = Vec::new();

    let status = run_agent_loop_with_persistence(
        &runtime,
        StepInput::user_text("cancel this run").expect("valid input"),
        &mut output,
        std::future::ready(Ok(())),
        HeadlessRunPersistence {
            loop_config: AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS)
                .expect("valid config"),
            context: StepContext::default(),
            events_jsonl: false,
            session_store: store.clone(),
            session_id: &session_id,
        },
    )
    .await
    .expect("an incomplete terminal run should persist");
    assert_eq!(status, RunExitStatus::Incomplete);

    drop(runtime);
    let transcript = resumed_transcript(&session_id, &workspace, store).await;
    assert_transcript_contains_user_input(&transcript, "cancel this run");
}

#[tokio::test(flavor = "current_thread")]
async fn incomplete_human_output_failure_still_persists() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let store = FileSessionStore::new(temp.path().join("sessions"));
    let session_id = SessionId::new("incomplete-output-save").expect("valid session id");
    let runtime = build_runtime(&session_id, &workspace, Arc::new(incomplete_provider()));

    let result = run_agent_loop_with_persistence(
        &runtime,
        StepInput::user_text("cancel with closed output").expect("valid input"),
        FailingWriter::new(io::ErrorKind::BrokenPipe, "closed human-output consumer"),
        std::future::ready(Ok(())),
        HeadlessRunPersistence {
            loop_config: AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS)
                .expect("valid config"),
            context: StepContext::default(),
            events_jsonl: false,
            session_store: store.clone(),
            session_id: &session_id,
        },
    )
    .await;
    assert!(matches!(result, Err(CliError::BrokenPipe)));

    drop(runtime);
    let transcript = resumed_transcript(&session_id, &workspace, store).await;
    assert_transcript_contains_user_input(&transcript, "cancel with closed output");
}

#[tokio::test(flavor = "current_thread")]
async fn output_error_precedes_reviewer_error_after_persistence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let store = FileSessionStore::new(temp.path().join("sessions"));
    let session_id = SessionId::new("output-review-ordering").expect("valid session id");
    let runtime = build_runtime(
        &session_id,
        &workspace,
        Arc::new(completing_provider("answer that cannot be presented")),
    );

    let result = run_agent_loop_with_persistence(
        &runtime,
        StepInput::user_text("ordering task").expect("valid input"),
        FailingWriter::new(io::ErrorKind::Other, "injected final output failure"),
        std::future::ready(Err(CliError::Unexpected(
            "injected reviewer presentation failure".to_owned(),
        ))),
        HeadlessRunPersistence {
            loop_config: AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS)
                .expect("valid config"),
            context: StepContext::default(),
            events_jsonl: false,
            session_store: store.clone(),
            session_id: &session_id,
        },
    )
    .await;
    match result {
        Err(CliError::Unexpected(message)) => {
            assert!(message.contains("injected final output failure"));
            assert!(!message.contains("reviewer"));
        }
        other => panic!("output failure should remain primary, got {other:?}"),
    }

    drop(runtime);
    let transcript = resumed_transcript(&session_id, &workspace, store).await;
    assert_transcript_contains_user_input(&transcript, "ordering task");
}

#[tokio::test(flavor = "current_thread")]
async fn reviewer_settles_before_persistence_and_its_error_is_returned_after_save() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let sessions_dir = temp.path().join("sessions");
    std::fs::write(&sessions_dir, "reviewer has not settled")
        .expect("the store should start invalid");
    let store = FileSessionStore::new(&sessions_dir);
    let session_id = SessionId::new("reviewer-error-save").expect("valid session id");
    let runtime = build_runtime(
        &session_id,
        &workspace,
        Arc::new(completing_provider("completed before reviewer failure")),
    );
    let mut output = Vec::new();

    let result = run_agent_loop_with_persistence(
        &runtime,
        StepInput::user_text("reviewer failure task").expect("valid input"),
        &mut output,
        async {
            tokio::fs::remove_file(&sessions_dir)
                .await
                .expect("reviewer settlement should remove the invalid store fixture");
            tokio::fs::create_dir(&sessions_dir)
                .await
                .expect("reviewer settlement should make the store writable");
            Err(CliError::Unexpected(
                "injected reviewer presentation failure".to_owned(),
            ))
        },
        HeadlessRunPersistence {
            loop_config: AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS)
                .expect("valid config"),
            context: StepContext::default(),
            events_jsonl: false,
            session_store: store.clone(),
            session_id: &session_id,
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(CliError::Unexpected(message)) if message == "injected reviewer presentation failure"
    ));

    drop(runtime);
    let transcript = resumed_transcript(&session_id, &workspace, store).await;
    assert_transcript_contains_user_input(&transcript, "reviewer failure task");
}

#[tokio::test(flavor = "current_thread")]
async fn save_error_precedes_broken_pipe_and_reviewer_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let invalid_store_root = temp.path().join("store-is-a-file");
    std::fs::write(&invalid_store_root, "not a directory")
        .expect("invalid store fixture should be written");
    let session_id = SessionId::new("save-error-propagation").expect("valid session id");
    let runtime = build_runtime(
        &session_id,
        &workspace,
        Arc::new(completing_provider("completed before save failure")),
    );

    let result = run_agent_loop_with_persistence(
        &runtime,
        StepInput::user_text("must report save failure").expect("valid input"),
        FailingWriter::new(io::ErrorKind::BrokenPipe, "closed human-output consumer"),
        std::future::ready(Err(CliError::Unexpected(
            "injected reviewer presentation failure".to_owned(),
        ))),
        HeadlessRunPersistence {
            loop_config: AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS)
                .expect("valid config"),
            context: StepContext::default(),
            events_jsonl: false,
            session_store: FileSessionStore::new(invalid_store_root),
            session_id: &session_id,
        },
    )
    .await;
    match result {
        Err(CliError::Unexpected(message)) => {
            assert!(message.contains("session save-error-propagation could not be saved"));
            assert!(message.contains("session store IO error"));
        }
        other => panic!("save failure must not become BrokenPipe success, got {other:?}"),
    }
}
