use futures_util::StreamExt;
use merry_core::{
    ArtifactKind, RuntimeEvent, RuntimeEventKind, SessionId, ToolCallResult, ToolCallResultStatus,
    ToolName,
};
use merry_llm::{
    FinishReason, ModelEvent, ModelName, ModelOutput, ModelResponse, ModelToolCall,
    ModelToolCallId, ModelToolResultContent, ToolArguments, testing::FakeModelProvider,
};
use merry_runtime::{Runtime, StepContext, StepInput, ToolExecutionContext};
use merry_tool_workspace::{
    ReadOnlyWorkspaceTools, WORKSPACE_LIST_DIR_TOOL, WORKSPACE_PATCH_FILE_TOOL,
    WORKSPACE_READ_FILE_TOOL, WORKSPACE_SEARCH_TEXT_TOOL, WorkspaceToolsConfig,
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

    fn write_text(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory should be created");
        }
        fs::write(path, content).expect("workspace file should be written");
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

fn pending_workspace_call(
    call_id: &str,
    tool_name: &str,
    arguments: Map<String, Value>,
) -> ModelEvent {
    let call = ModelToolCall::new(
        ModelToolCallId::new(call_id).expect("valid model tool call id"),
        ToolName::new(tool_name).expect("valid tool name"),
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

fn pending_read_file_call(path: &str) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert("path".to_owned(), Value::String(path.to_owned()));
    pending_workspace_call("workspace-read-call", WORKSPACE_READ_FILE_TOOL, arguments)
}

fn pending_list_dir_call(path: &str) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert("path".to_owned(), Value::String(path.to_owned()));
    pending_workspace_call("workspace-list-call", WORKSPACE_LIST_DIR_TOOL, arguments)
}

fn pending_search_text_call(path: &str, query: &str) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert("path".to_owned(), Value::String(path.to_owned()));
    arguments.insert("query".to_owned(), Value::String(query.to_owned()));
    pending_workspace_call(
        "workspace-search-call",
        WORKSPACE_SEARCH_TEXT_TOOL,
        arguments,
    )
}

fn pending_patch_file_call(path: &str, old_text: &str, new_text: &str) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert("path".to_owned(), Value::String(path.to_owned()));
    arguments.insert("old_text".to_owned(), Value::String(old_text.to_owned()));
    arguments.insert("new_text".to_owned(), Value::String(new_text.to_owned()));
    pending_workspace_call("workspace-patch-call", WORKSPACE_PATCH_FILE_TOOL, arguments)
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

fn runtime_with_workspace_tools(root: &Path, model_event: ModelEvent) -> Runtime {
    let (runtime, _) = runtime_with_workspace_tools_and_provider(root, model_event);
    runtime
}

fn runtime_with_workspace_tools_and_provider(
    root: &Path,
    model_event: ModelEvent,
) -> (Runtime, FakeModelProvider) {
    let tools = ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![root.to_path_buf()]))
        .expect("workspace tools should construct");
    let provider = FakeModelProvider::new(vec![Ok(model_event)]);
    let provider_handle = provider.clone();
    let mut builder =
        Runtime::builder(session_id()).model_provider(Arc::new(provider), model_name());
    for tool in tools.into_registered_tools() {
        builder = builder.register_tool(tool);
    }
    (
        builder.build().expect("runtime should build"),
        provider_handle,
    )
}

fn runtime_with_workspace_patch_tools(root: &Path, model_event: ModelEvent) -> Runtime {
    let tools = ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![root.to_path_buf()]))
        .expect("workspace tools should construct");
    let provider = FakeModelProvider::new(vec![Ok(model_event)]);
    let mut builder =
        Runtime::builder(session_id()).model_provider(Arc::new(provider), model_name());
    for tool in tools.into_registered_tools_with_patch() {
        builder = builder.register_tool(tool);
    }
    builder.build().expect("runtime should build")
}

async fn execute_first_pending_call(runtime: &Runtime, user_text: &str) -> Vec<RuntimeEvent> {
    let pending_events = collect_step(runtime, user_text).await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending_calls = runtime.pending_tool_calls().await;
    assert_eq!(pending_calls.len(), 1);
    let pending = pending_calls
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("registered workspace tool should execute");

    assert!(runtime.pending_tool_calls().await.is_empty());
    execution_events
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

fn resolved_tool_result(events: &[RuntimeEvent]) -> &ToolCallResult {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("tool resolution event should be present")
}

fn assert_artifact_recorded_before_tool_resolution(
    events: &[RuntimeEvent],
    result: &ToolCallResult,
) {
    assert_eq!(
        event_kind_names(events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.kind, RuntimeEventKind::Failed { .. })),
        "tool execution should not emit RuntimeEventKind::Failed: {events:?}"
    );
    assert!(matches!(
        &events[0].kind,
        RuntimeEventKind::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &events[1].kind,
        RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == result
    ));
}

fn assert_succeeded_json_result(events: &[RuntimeEvent]) {
    let result = resolved_tool_result(events);
    assert_artifact_recorded_before_tool_resolution(events, result);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert!(result.diagnostic().is_none());
}

fn assert_failed_json_result(events: &[RuntimeEvent], diagnostic_code: &str) {
    let result = resolved_tool_result(events);
    assert_artifact_recorded_before_tool_resolution(events, result);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should include diagnostic")
            .code(),
        diagnostic_code
    );
}

async fn assert_failed_json_artifact_visible_in_next_model_request(
    runtime: &Runtime,
    provider: &FakeModelProvider,
    expected_tool: &str,
    expected_code: &str,
    host_root: &Path,
) {
    let _events = collect_step(runtime, "continue after tool failure").await;

    let requests = provider.recorded_requests();
    let request = requests
        .last()
        .expect("follow-up request should be recorded");
    let continuation = request
        .continuations()
        .last()
        .expect("failed tool result should be sent as continuation");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .expect("failed continuation should include diagnostic")
            .code(),
        expected_code
    );

    let ModelToolResultContent::Json(json) = continuation.result().content() else {
        panic!("failed workspace result should be JSON");
    };
    let payload: Value = serde_json::from_str(json).expect("failed result JSON should parse");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["tool"], expected_tool);
    assert_eq!(payload["error"]["code"], expected_code);
    assert!(
        !json.contains(host_root.to_str().expect("temp path utf8")),
        "failed JSON artifact must not include absolute host roots"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn registered_read_file_tool_records_artifact_before_resolving_pending_call() {
    let temp = TempWorkspace::new("event-order");
    temp.write_text("note.txt", "alpha\n");
    let runtime = runtime_with_workspace_tools(temp.path(), pending_read_file_call("note.txt"));

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
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn registered_read_file_domain_failure_records_failed_json_before_resolving_pending_call() {
    let temp = TempWorkspace::new("domain-failure");
    let runtime = runtime_with_workspace_tools(temp.path(), pending_read_file_call("missing.txt"));

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

#[tokio::test(flavor = "current_thread")]
async fn registered_list_dir_tool_records_json_artifact_before_resolving_pending_call() {
    let temp = TempWorkspace::new("list-success");
    temp.write_text("notes/alpha.txt", "alpha\n");
    temp.write_text("notes/nested/beta.txt", "beta\n");
    let runtime = runtime_with_workspace_tools(temp.path(), pending_list_dir_call("notes"));

    let execution_events = execute_first_pending_call(&runtime, "list notes").await;

    assert_succeeded_json_result(&execution_events);
}

#[tokio::test(flavor = "current_thread")]
async fn registered_list_dir_domain_failure_records_failed_json_without_runtime_failed() {
    let temp = TempWorkspace::new("list-domain-failure");
    temp.write_text("notes/alpha.txt", "alpha\n");
    let (runtime, provider) =
        runtime_with_workspace_tools_and_provider(temp.path(), pending_list_dir_call("../outside"));

    let execution_events = execute_first_pending_call(&runtime, "list outside").await;

    assert_failed_json_result(&execution_events, "workspace_path_denied");
    assert_failed_json_artifact_visible_in_next_model_request(
        &runtime,
        &provider,
        WORKSPACE_LIST_DIR_TOOL,
        "workspace_path_denied",
        temp.path(),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn registered_search_text_tool_records_json_artifact_before_resolving_pending_call() {
    let temp = TempWorkspace::new("search-success");
    temp.write_text("notes/alpha.txt", "first alpha\n");
    temp.write_text("notes/nested/beta.txt", "second alpha\n");
    temp.write_text("notes/nested/gamma.txt", "unmatched\n");
    let runtime =
        runtime_with_workspace_tools(temp.path(), pending_search_text_call("notes", "alpha"));

    let execution_events = execute_first_pending_call(&runtime, "search notes").await;

    assert_succeeded_json_result(&execution_events);
}

#[tokio::test(flavor = "current_thread")]
async fn registered_search_text_domain_failure_records_failed_json_without_runtime_failed() {
    let temp = TempWorkspace::new("search-domain-failure");
    temp.write_text("notes/alpha.txt", "alpha\n");
    let (runtime, provider) = runtime_with_workspace_tools_and_provider(
        temp.path(),
        pending_search_text_call("../outside", "alpha"),
    );

    let execution_events = execute_first_pending_call(&runtime, "search outside").await;

    assert_failed_json_result(&execution_events, "workspace_path_denied");
    assert_failed_json_artifact_visible_in_next_model_request(
        &runtime,
        &provider,
        WORKSPACE_SEARCH_TEXT_TOOL,
        "workspace_path_denied",
        temp.path(),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn registered_patch_file_tool_is_policy_denied_before_mutating_file() {
    let temp = TempWorkspace::new("patch-policy-denied");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let runtime = runtime_with_workspace_patch_tools(
        temp.path(),
        pending_patch_file_call("note.txt", "old", "new"),
    );

    let pending_events = collect_step(&runtime, "patch note").await;
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
        .expect("runtime policy denial should resolve pending call");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        "action_policy_denied"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).expect("workspace file should read"),
        "alpha\nold\nomega\n"
    );
}
