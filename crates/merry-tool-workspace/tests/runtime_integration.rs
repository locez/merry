use futures_util::StreamExt;
use merry_core::{
    ArtifactKind, ProviderName, RuntimeEvent, RuntimeEventKind, SessionId, ToolCallResult,
    ToolCallResultStatus, ToolName,
};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ModelToolResultContent, ToolArguments,
    testing::FakeModelProvider,
};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AgentLoopStatus,
    DEFAULT_AGENT_LOOP_CONTINUATION_INPUT, LedgerFactKind, LedgerProjection, ProcessActionIntent,
    ProcessExitStatus, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
    ProcessRunnerFuture, ProcessRunnerOutput, Runtime, StepContext, StepInput,
    ToolExecutionContext, process_command_tool,
};
use merry_tool_workspace::{
    ReadOnlyWorkspaceTools, WORKSPACE_LIST_DIR_TOOL, WORKSPACE_PATCH_FILE_TOOL,
    WORKSPACE_READ_FILE_TOOL, WORKSPACE_SEARCH_TEXT_TOOL, WorkspaceToolsConfig,
};
use serde_json::{Map, Value};
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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

fn pending_process_call(call_id: &str, argv: &[&str]) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert(
        "argv".to_owned(),
        Value::Array(
            argv.iter()
                .map(|argument| Value::String((*argument).to_owned()))
                .collect(),
        ),
    );
    pending_workspace_call(call_id, "run_process", arguments)
}

type ScriptedModelStep = Vec<Result<ModelEvent, ModelError>>;
type ScriptedModelSteps = Vec<ScriptedModelStep>;
type RecordedModelRequests = Vec<ModelRequest>;

#[derive(Debug, Clone)]
struct ScriptedModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Arc<Mutex<ScriptedModelSteps>>,
    recorded_requests: Arc<Mutex<RecordedModelRequests>>,
}

impl ScriptedModelProvider {
    fn new(steps: ScriptedModelSteps) -> Self {
        Self {
            name: ProviderName::new("workspace-scripted-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            steps: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded_requests(&self) -> RecordedModelRequests {
        self.recorded_requests
            .lock()
            .expect("recorded requests mutex should not be poisoned")
            .clone()
    }
}

impl ModelProvider for ScriptedModelProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        request: merry_llm::ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            self.recorded_requests
                .lock()
                .expect("recorded requests mutex should not be poisoned")
                .push(request);

            let script = self
                .steps
                .lock()
                .expect("steps mutex should not be poisoned")
                .pop()
                .unwrap_or_default();
            let stream: ModelEventStream = Box::pin(futures_util::stream::iter(script));
            Ok(stream)
        })
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

fn continuation_input_for(original_task: &str) -> String {
    format!("{DEFAULT_AGENT_LOOP_CONTINUATION_INPUT}\n\nOriginal task:\n{original_task}")
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

fn runtime_with_opt_in_workspace_patch_tools(root: &Path, model_event: ModelEvent) -> Runtime {
    let tools = ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![root.to_path_buf()]))
        .expect("workspace tools should construct");
    let provider = FakeModelProvider::new(vec![Ok(model_event)]);
    let mut builder = Runtime::builder(session_id())
        .model_provider(Arc::new(provider), model_name())
        .allow_low_risk_workspace_patches();
    for tool in tools.into_registered_tools_with_patch() {
        builder = builder.register_tool(tool);
    }
    builder.build().expect("runtime should build")
}

fn runtime_with_opt_in_workspace_patch_tools_and_provider(
    root: &Path,
    provider: ScriptedModelProvider,
) -> Runtime {
    let tools = ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![root.to_path_buf()]))
        .expect("workspace tools should construct");
    let mut builder = Runtime::builder(session_id())
        .model_provider(Arc::new(provider), model_name())
        .allow_low_risk_workspace_patches();
    for tool in tools.into_registered_tools_with_patch() {
        builder = builder.register_tool(tool);
    }
    builder.build().expect("runtime should build")
}

fn runtime_with_coding_loop_tools(
    root: &Path,
    provider: ScriptedModelProvider,
    runner: Arc<dyn ProcessRunner>,
) -> Runtime {
    let tools = ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![root.to_path_buf()]))
        .expect("workspace tools should construct");
    let mut builder = Runtime::builder(session_id())
        .model_provider(Arc::new(provider), model_name())
        .allow_low_risk_workspace_patches()
        .allow_low_risk_process_actions(runner.clone())
        .allow_accepted_local_workspace_process_actions(
            AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
            runner,
        )
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a local process from argv through runtime policy",
            )
            .expect("process command tool should build"),
        );
    for tool in tools.into_registered_tools_with_patch() {
        builder = builder.register_tool(tool);
    }
    builder.build().expect("runtime should build")
}

#[derive(Clone)]
struct ScriptedProcessRunner {
    observed_intents: Arc<Mutex<Vec<ProcessActionIntent>>>,
    responses: Arc<Mutex<Vec<ScriptedProcessResponse>>>,
}

#[derive(Clone)]
struct ScriptedProcessResponse {
    status: ProcessExitStatus,
    stdout_text: String,
    stderr_text: String,
}

impl ScriptedProcessRunner {
    fn new(responses: Vec<ScriptedProcessResponse>) -> Self {
        Self {
            observed_intents: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses.into_iter().rev().collect())),
        }
    }

    fn observed_intents(&self) -> Vec<ProcessActionIntent> {
        self.observed_intents
            .lock()
            .expect("process intents mutex should not be poisoned")
            .clone()
    }
}

impl ScriptedProcessResponse {
    fn success(stdout_text: &str) -> Self {
        Self {
            status: ProcessExitStatus::Exited(0),
            stdout_text: stdout_text.to_owned(),
            stderr_text: String::new(),
        }
    }
}

impl ProcessRunner for ScriptedProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ProcessRunnerError::Cancelled);
            }

            self.observed_intents
                .lock()
                .expect("process intents mutex should not be poisoned")
                .push(intent.clone());

            let response = self
                .responses
                .lock()
                .expect("process responses mutex should not be poisoned")
                .pop()
                .expect("scripted process response should exist");

            ProcessRunnerOutput::new(
                &intent,
                response.status,
                response.stdout_text,
                false,
                response.stderr_text,
                false,
            )
            .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
        })
    }
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

fn lifecycle_kinds(projection: &merry_runtime::LedgerProjectionSnapshot) -> Vec<LedgerFactKind> {
    projection
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            LedgerProjection::Lifecycle { kind, .. } => Some(*kind),
            LedgerProjection::Fact { .. } => None,
        })
        .collect()
}

fn assert_successful_patch_content_does_not_leak_internal_metadata(json: &str) {
    let payload: Value = serde_json::from_str(json).expect("patch result JSON should parse");
    assert_eq!(payload["ok"], true);
    for forbidden_key in [
        "proposal",
        "audit",
        "evidence",
        "fingerprint",
        "preimage_bytes",
        "replacement_bytes",
        "file_fingerprint_before",
        "file_fingerprint_after",
    ] {
        assert_json_key_absent(&payload, forbidden_key);
    }
    for forbidden_text in [
        "proposal",
        "audit",
        "evidence",
        "fingerprint",
        "fnv1a64",
        "preimage_bytes",
        "replacement_bytes",
        "file_bytes_before",
        "file_bytes_after",
    ] {
        assert!(
            !json.contains(forbidden_text),
            "successful patch result leaked {forbidden_text}: {json}"
        );
    }
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

fn assert_json_key_absent(value: &Value, key: &str) {
    match value {
        Value::Object(map) => {
            assert!(map.get(key).is_none(), "sanitized JSON leaked key {key}");
            for nested in map.values() {
                assert_json_key_absent(nested, key);
            }
        }
        Value::Array(values) => {
            for nested in values {
                assert_json_key_absent(nested, key);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn assert_patch_denial_json_sanitized(json: &str, expected_tool: &str) {
    let payload: Value = serde_json::from_str(json).expect("denied result JSON should parse");
    assert_eq!(
        payload,
        serde_json::json!({
            "ok": false,
            "tool": expected_tool,
            "error": {
                "code": "action_policy_denied",
                "message": "tool action was blocked by runtime policy"
            }
        })
    );

    for forbidden_key in [
        "proposal",
        "audit",
        "action_kind",
        "policy",
        "reason",
        "relative_path",
        "preimage_bytes",
        "replacement_bytes",
        "file_bytes_before",
        "file_bytes_after",
        "risk",
        "internal",
        "provider",
        "provider_response",
        "wire",
        "previous_response_id",
    ] {
        assert_json_key_absent(&payload, forbidden_key);
    }

    for forbidden_text in [
        "proposal",
        "audit",
        "action_kind",
        "relative_path",
        "preimage_bytes",
        "replacement_bytes",
        "file_bytes_before",
        "file_bytes_after",
        "risk",
        "internal",
        "previous_response_id",
        "OpenAI",
        "wire",
    ] {
        assert!(
            !json.contains(forbidden_text),
            "sanitized denied result leaked {forbidden_text}: {json}"
        );
    }
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

#[tokio::test(flavor = "current_thread")]
async fn denied_patch_file_leaves_file_unchanged_and_returns_sanitized_result() {
    let temp = TempWorkspace::new("patch-policy-proposed");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let runtime = runtime_with_workspace_patch_tools(
        temp.path(),
        pending_patch_file_call("note.txt", "old", "new"),
    );
    let _pending_events = collect_step(&runtime, "patch note").await;
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
        fs::read_to_string(temp.path().join("note.txt")).expect("workspace file should read"),
        "alpha\nold\nomega\n"
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
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_patch_file_tool_applies_patch_and_records_artifact_before_resolution() {
    let temp = TempWorkspace::new("patch-opt-in-success");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let runtime = runtime_with_opt_in_workspace_patch_tools(
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
        .expect("opted-in workspace patch should execute");

    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).expect("workspace file should read"),
        "alpha\nnew\nomega\n"
    );
    assert_succeeded_json_result(&execution_events);
    let lifecycle = lifecycle_kinds(&runtime.ledger_projection().await);
    let audit_indexes = lifecycle
        .iter()
        .enumerate()
        .filter_map(|(index, kind)| (*kind == LedgerFactKind::ActionAuditRecorded).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(audit_indexes.len(), 2);
    let artifact_index = lifecycle
        .iter()
        .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
        .expect("artifact lifecycle should exist");
    let resolved_index = lifecycle
        .iter()
        .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
        .expect("resolution lifecycle should exist");
    assert!(audit_indexes[0] < audit_indexes[1]);
    assert!(audit_indexes[1] < artifact_index);
    assert!(artifact_index < resolved_index);
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_patch_success_continuation_does_not_leak_internal_evidence() {
    let temp = TempWorkspace::new("patch-opt-in-no-leak");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(pending_patch_file_call("note.txt", "old", "new"))],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("continued after patch")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let provider_handle = provider.clone();
    let runtime = runtime_with_opt_in_workspace_patch_tools_and_provider(temp.path(), provider);
    let _pending_events = collect_step(&runtime, "patch note").await;
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("opted-in workspace patch should execute");
    assert_succeeded_json_result(&execution_events);

    let _continuation_events = collect_step(&runtime, "continue after patch").await;
    let requests = provider_handle.recorded_requests();
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("successful tool result should be compiled as continuation");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    let ModelToolResultContent::Json(continuation_json) = continuation.result().content() else {
        panic!("successful patch continuation should be JSON");
    };
    assert_successful_patch_content_does_not_leak_internal_metadata(continuation_json);
}

#[tokio::test(flavor = "current_thread")]
async fn patch_proposal_and_audit_do_not_leak_into_sanitized_result_or_continuation() {
    let temp = TempWorkspace::new("patch-policy-no-leak");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools =
        ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]))
            .expect("workspace tools should construct");
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(pending_patch_file_call("note.txt", "old", "new"))],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("continued after denial")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let provider_handle = provider.clone();
    let mut builder =
        Runtime::builder(session_id()).model_provider(Arc::new(provider), model_name());
    for tool in tools.into_registered_tools_with_patch() {
        builder = builder.register_tool(tool);
    }
    let runtime = builder.build().expect("runtime should build");
    let _pending_events = collect_step(&runtime, "patch note").await;
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

    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        "action_policy_denied"
    );

    let _continuation_events = collect_step(&runtime, "continue after denial").await;
    let requests = provider_handle.recorded_requests();
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("denied tool result should be compiled as continuation");
    let ModelToolResultContent::Json(continuation_json) = continuation.result().content() else {
        panic!("denial continuation should be JSON");
    };
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .expect("denial continuation should include diagnostic")
            .code(),
        "action_policy_denied"
    );
    assert_patch_denial_json_sanitized(continuation_json, WORKSPACE_PATCH_FILE_TOOL);
}

#[tokio::test(flavor = "current_thread")]
async fn coding_loop_harness_inspects_patches_verifies_and_completes() {
    let temp = TempWorkspace::new("coding-loop-harness");
    temp.write_text(
        "src/lib.rs",
        "pub fn greeting() -> &'static str {\n    \"old\"\n}\n",
    );
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(pending_process_call(
            "coding-loop-rg-files",
            &["rg", "--files"],
        ))],
        vec![Ok(pending_read_file_call("src/lib.rs"))],
        vec![Ok(pending_patch_file_call(
            "src/lib.rs",
            "\"old\"",
            "\"new\"",
        ))],
        vec![Ok(pending_process_call(
            "coding-loop-cargo-test",
            &["cargo", "test", "-p", "merry-runtime"],
        ))],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text(
                    "changed greeting to new and verified tests",
                )],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let provider_handle = provider.clone();
    let runner = Arc::new(ScriptedProcessRunner::new(vec![
        ScriptedProcessResponse::success("src/lib.rs\n"),
        ScriptedProcessResponse::success("test result: ok. 1 passed; 0 failed\n"),
    ]));
    let runtime = runtime_with_coding_loop_tools(temp.path(), provider, runner.clone());

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Fix the greeting and verify it.").expect("valid user task"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(8).expect("valid step budget"),
        )
        .await
        .expect("coding loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.steps_run(), 5);
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(
        fs::read_to_string(temp.path().join("src/lib.rs"))
            .expect("patched workspace file should read"),
        "pub fn greeting() -> &'static str {\n    \"new\"\n}\n"
    );

    let observed_argv = runner
        .observed_intents()
        .into_iter()
        .map(|intent| intent.argv().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        observed_argv,
        vec![
            vec!["rg".to_owned(), "--files".to_owned()],
            vec![
                "cargo".to_owned(),
                "test".to_owned(),
                "-p".to_owned(),
                "merry-runtime".to_owned()
            ],
        ]
    );

    let requests = provider_handle.recorded_requests();
    assert_eq!(requests.len(), 5);
    assert!(requests[0].continuations().is_empty());
    let expected_continuation_input = continuation_input_for("Fix the greeting and verify it.");
    for request in requests.iter().skip(1) {
        assert_eq!(
            request.messages()[0].content().as_text(),
            expected_continuation_input
        );
    }
    let continuation_ids = requests
        .iter()
        .skip(1)
        .map(|request| {
            assert_eq!(request.continuations().len(), 1);
            request.continuations()[0].call().id().as_str().to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        continuation_ids,
        [
            "coding-loop-rg-files",
            "workspace-read-call",
            "workspace-patch-call",
            "coding-loop-cargo-test",
        ]
    );

    let lifecycle = lifecycle_kinds(&runtime.ledger_projection().await);
    let artifact_indexes = lifecycle
        .iter()
        .enumerate()
        .filter_map(|(index, kind)| (*kind == LedgerFactKind::ArtifactRecorded).then_some(index))
        .collect::<Vec<_>>();
    let resolved_indexes = lifecycle
        .iter()
        .enumerate()
        .filter_map(|(index, kind)| (*kind == LedgerFactKind::ToolCallResolved).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(artifact_indexes.len(), 5);
    assert_eq!(resolved_indexes.len(), 4);
    for (artifact_index, resolved_index) in artifact_indexes
        .iter()
        .take(resolved_indexes.len())
        .zip(resolved_indexes.iter())
    {
        assert!(
            artifact_index < resolved_index,
            "tool result artifact must be recorded before tool resolution"
        );
    }
}
