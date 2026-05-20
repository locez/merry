use futures_util::StreamExt;
use merry_core::{
    ArtifactKind, RuntimeEvent, RuntimeEventKind, SessionId, ToolCallResultStatus, ToolName,
};
use merry_llm::{
    FinishReason, ModelEvent, ModelName, ModelOutput, ModelResponse, ModelToolCall,
    ModelToolCallId, ToolArguments, testing::FakeModelProvider,
};
use merry_runtime::{Runtime, StepContext, StepInput, ToolExecutionContext};
use merry_tool_workspace::{
    ReadOnlyWorkspaceTools, WORKSPACE_READ_FILE_TOOL, WorkspaceToolsConfig,
};
use serde_json::{Map, Value};
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new(name: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "merry-tool-workspace-runtime-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("temp workspace should be created");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn session_id() -> SessionId {
    SessionId::new("workspace-tool-runtime").expect("valid session id")
}

fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

fn pending_read_file_call(path: &str) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert("path".to_owned(), Value::String(path.to_owned()));
    let call = ModelToolCall::new(
        ModelToolCallId::new("workspace-read-call").expect("valid model tool call id"),
        ToolName::new(WORKSPACE_READ_FILE_TOOL).expect("valid tool name"),
        ToolArguments::new(arguments),
    );
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    }
}

async fn collect_step(runtime: &Runtime, text: &str) -> Vec<RuntimeEvent> {
    runtime
        .step(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("step should start")
        .collect()
        .await
}

fn event_kind_names(events: &[RuntimeEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event.kind {
            RuntimeEventKind::SessionStarted => "SessionStarted",
            RuntimeEventKind::StepStarted => "StepStarted",
            RuntimeEventKind::StepCompleted => "StepCompleted",
            RuntimeEventKind::ArtifactRecorded { .. } => "ArtifactRecorded",
            RuntimeEventKind::EvidenceReferenced { .. } => "EvidenceReferenced",
            RuntimeEventKind::ToolCallPending { .. } => "ToolCallPending",
            RuntimeEventKind::ToolCallResolved { .. } => "ToolCallResolved",
            RuntimeEventKind::Cancelled { .. } => "Cancelled",
            RuntimeEventKind::Failed { .. } => "Failed",
            _ => "Other",
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn registered_read_file_tool_records_artifact_before_resolving_pending_call() {
    let temp = TempWorkspace::new("event-order");
    fs::write(temp.path().join("note.txt"), "alpha\n").expect("workspace file should be written");
    let tools =
        ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]))
            .expect("workspace tools should construct");
    let provider = FakeModelProvider::new(vec![Ok(pending_read_file_call("note.txt"))]);

    let runtime = Runtime::builder(session_id())
        .model_provider(Arc::new(provider), model_name())
        .register_tool(
            tools
                .into_registered_tools()
                .into_iter()
                .next()
                .expect("read tool should be registered"),
        )
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "read note").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("registered read file tool should execute");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = match &execution_events[1].kind {
        RuntimeEventKind::ToolCallResolved { result } => result,
        other => panic!("expected tool resolution, got {other:?}"),
    };
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
}

#[tokio::test(flavor = "current_thread")]
async fn registered_read_file_domain_failure_records_failed_json_before_resolving_pending_call() {
    let temp = TempWorkspace::new("domain-failure");
    let tools =
        ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]))
            .expect("workspace tools should construct");
    let provider = FakeModelProvider::new(vec![Ok(pending_read_file_call("missing.txt"))]);

    let runtime = Runtime::builder(session_id())
        .model_provider(Arc::new(provider), model_name())
        .register_tool(
            tools
                .into_registered_tools()
                .into_iter()
                .next()
                .expect("read tool should be registered"),
        )
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "read missing note").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("domain failure should resolve pending call");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = match &execution_events[1].kind {
        RuntimeEventKind::ToolCallResolved { result } => result,
        other => panic!("expected tool resolution, got {other:?}"),
    };
    assert!(matches!(
        &execution_events[0].kind,
        RuntimeEventKind::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should include diagnostic")
            .code(),
        "workspace_file_not_found"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}
