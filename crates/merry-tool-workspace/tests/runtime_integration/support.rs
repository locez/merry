pub(super) use futures_util::StreamExt;
pub(super) use merry_core::{
    ArtifactKind, ProviderName, RuntimeJournalEvent, RuntimeJournalPayload, SessionId,
    ToolCallResult, ToolCallResultStatus, ToolName,
};
pub(super) use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ModelToolResultContent, ToolArguments,
    testing::FakeModelProvider,
};
pub(super) use merry_runtime::{
    LedgerFactKind, LedgerProjection, Runtime, StepContext, StepInput, ToolExecutionContext,
};
pub(super) use merry_tool_workspace::{
    ReadOnlyWorkspaceTools, WORKSPACE_LIST_DIR_TOOL, WORKSPACE_PATCH_TOOL,
    WORKSPACE_READ_FILE_TOOL, WORKSPACE_SEARCH_TEXT_TOOL, WorkspaceToolsConfig,
};
pub(super) use serde_json::{Map, Value};
pub(super) use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
pub(super) use tokio_util::sync::CancellationToken;

pub(super) struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    pub(super) fn new(name: &str) -> Self {
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

    pub(super) fn path(&self) -> &Path {
        &self.root
    }

    pub(super) fn write_text(&self, relative: &str, content: &str) {
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

pub(super) fn session_id() -> SessionId {
    SessionId::new("workspace-tool-runtime").expect("valid session id")
}

pub(super) fn model_name() -> ModelName {
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

pub(super) fn pending_read_file_call(path: &str) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert("path".to_owned(), Value::String(path.to_owned()));
    pending_workspace_call("workspace-read-call", WORKSPACE_READ_FILE_TOOL, arguments)
}

pub(super) fn pending_list_dir_call(path: &str) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert("path".to_owned(), Value::String(path.to_owned()));
    pending_workspace_call("workspace-list-call", WORKSPACE_LIST_DIR_TOOL, arguments)
}

pub(super) fn pending_search_text_call(path: &str, query: &str) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert("path".to_owned(), Value::String(path.to_owned()));
    arguments.insert("query".to_owned(), Value::String(query.to_owned()));
    pending_workspace_call(
        "workspace-search-call",
        WORKSPACE_SEARCH_TEXT_TOOL,
        arguments,
    )
}

pub(super) fn pending_patch_call(path: &str, old_text: &str, new_text: &str) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert(
        "patch".to_owned(),
        Value::String(update_patch(path, old_text, new_text)),
    );
    pending_workspace_call("workspace-patch-call", WORKSPACE_PATCH_TOOL, arguments)
}

pub(super) fn pending_add_patch_call(path: &str, lines: &[&str]) -> ModelEvent {
    let mut arguments = Map::new();
    arguments.insert("patch".to_owned(), Value::String(add_patch(path, lines)));
    pending_workspace_call("workspace-patch-call", WORKSPACE_PATCH_TOOL, arguments)
}

pub(super) fn update_patch(path: &str, old_text: &str, new_text: &str) -> String {
    format!(
        "*** Begin Workspace Patch\n*** Update File: {path}\n-{old_text}\n+{new_text}\n*** End Workspace Patch"
    )
}

pub(super) fn add_patch(path: &str, lines: &[&str]) -> String {
    let additions = lines
        .iter()
        .map(|line| format!("+{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("*** Begin Workspace Patch\n*** Add File: {path}\n{additions}\n*** End Workspace Patch")
}

type ScriptedModelStep = Vec<Result<ModelEvent, ModelError>>;
type ScriptedModelSteps = Vec<ScriptedModelStep>;
type RecordedModelRequests = Vec<ModelRequest>;

#[derive(Debug, Clone)]
pub(super) struct ScriptedModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Arc<Mutex<ScriptedModelSteps>>,
    recorded_requests: Arc<Mutex<RecordedModelRequests>>,
}

impl ScriptedModelProvider {
    pub(super) fn new(steps: ScriptedModelSteps) -> Self {
        Self {
            name: ProviderName::new("workspace-scripted-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            steps: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(super) fn recorded_requests(&self) -> RecordedModelRequests {
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

pub(super) async fn collect_step(runtime: &Runtime, text: &str) -> Vec<RuntimeJournalEvent> {
    runtime
        .step(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("step should start")
        .collect()
        .await
}

pub(super) fn runtime_with_workspace_tools(root: &Path, model_event: ModelEvent) -> Runtime {
    let (runtime, _) = runtime_with_workspace_tools_and_provider(root, model_event);
    runtime
}

pub(super) fn runtime_with_workspace_tools_and_provider(
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

pub(super) fn runtime_with_workspace_patch_tools(root: &Path, model_event: ModelEvent) -> Runtime {
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

pub(super) fn runtime_with_opt_in_workspace_patch_tools(
    root: &Path,
    model_event: ModelEvent,
) -> Runtime {
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

pub(super) fn runtime_with_opt_in_workspace_patch_tools_and_provider(
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

pub(super) async fn execute_first_pending_call(
    runtime: &Runtime,
    user_text: &str,
) -> Vec<RuntimeJournalEvent> {
    let pending_events = collect_step(runtime, user_text).await;
    assert_pending_tool_call_events(&pending_events);
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

fn assert_pending_tool_call_events(events: &[RuntimeJournalEvent]) {
    let kinds = event_kind_names(events);
    assert!(
        kinds == ["SessionStarted", "StepStarted", "ToolCallPending"]
            || kinds
                == [
                    "SessionStarted",
                    "StepStarted",
                    "ModelRetryAttemptStarted",
                    "ToolCallPending"
                ],
        "unexpected pending tool call event sequence: {kinds:?}"
    );
}

pub(super) fn event_kind_names(events: &[RuntimeJournalEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event.payload {
            RuntimeJournalPayload::SessionStarted => "SessionStarted",
            RuntimeJournalPayload::StepStarted => "StepStarted",
            RuntimeJournalPayload::ModelRetryAttemptStarted { .. } => "ModelRetryAttemptStarted",
            RuntimeJournalPayload::ModelRetryScheduled { .. } => "ModelRetryScheduled",
            RuntimeJournalPayload::ModelRetryExhausted { .. } => "ModelRetryExhausted",
            RuntimeJournalPayload::StepCompleted => "StepCompleted",
            RuntimeJournalPayload::ArtifactRecorded { .. } => "ArtifactRecorded",
            RuntimeJournalPayload::AssistantOutputRecorded { .. } => "AssistantOutputRecorded",
            RuntimeJournalPayload::EvidenceReferenced { .. } => "EvidenceReferenced",
            RuntimeJournalPayload::ToolCallPending { .. } => "ToolCallPending",
            RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
            RuntimeJournalPayload::Cancelled { .. } => "Cancelled",
            RuntimeJournalPayload::Failed { .. } => "Failed",
            _ => "Other",
        })
        .collect()
}

pub(super) fn resolved_tool_result(events: &[RuntimeJournalEvent]) -> &ToolCallResult {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("tool resolution event should be present")
}

fn assert_artifact_recorded_before_tool_resolution(
    events: &[RuntimeJournalEvent],
    result: &ToolCallResult,
) {
    assert_eq!(
        event_kind_names(events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::Failed { .. })),
        "tool execution should not emit RuntimeJournalPayload::Failed: {events:?}"
    );
    assert!(matches!(
        &events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &events[1].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
}

pub(super) fn assert_succeeded_json_result(events: &[RuntimeJournalEvent]) {
    let result = resolved_tool_result(events);
    assert_artifact_recorded_before_tool_resolution(events, result);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert!(result.diagnostic().is_none());
}

pub(super) fn lifecycle_kinds(
    projection: &merry_runtime::LedgerProjectionSnapshot,
) -> Vec<LedgerFactKind> {
    projection
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            LedgerProjection::Lifecycle { kind, .. } => Some(*kind),
            LedgerProjection::Fact { .. } => None,
        })
        .collect()
}

pub(super) fn assert_successful_patch_content_does_not_leak_internal_metadata(json: &str) {
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

pub(super) fn assert_failed_json_result(events: &[RuntimeJournalEvent], diagnostic_code: &str) {
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

pub(super) fn assert_json_key_absent(value: &Value, key: &str) {
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

pub(super) fn assert_patch_denial_json_sanitized(json: &str, expected_tool: &str) {
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

pub(super) async fn assert_failed_json_artifact_visible_in_next_model_request(
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
        payload["recovery"]["path_contract"]
            .as_str()
            .expect("recovery path contract should be text")
            .contains("relative to a configured workspace root")
    );
    assert!(
        !json.contains(host_root.to_str().expect("temp path utf8")),
        "failed JSON artifact must not include absolute host roots"
    );
}
