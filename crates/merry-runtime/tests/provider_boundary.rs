use futures_util::{StreamExt, stream};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, EvidenceLocator, PendingToolCall,
    ProviderName, RuntimeJournalEvent, RuntimeJournalPayload, SessionId, ToolCallId,
    ToolCallResult, ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelCapabilities, ModelContent, ModelError, ModelEvent,
    ModelEventStream, ModelInputItem, ModelMessage, ModelMessageRole, ModelName, ModelOutput,
    ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse, ModelResponseFormat,
    ModelStreamContext, ModelStructuredOutputFormat, ModelToolCall, ModelToolCallId,
    ProviderErrorKind, ToolArguments, testing::FakeModelProvider,
};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use merry_runtime::{
    ArtifactContent, ArtifactContentKind, ArtifactError, CheckpointId, CheckpointRef,
    CheckpointRefId, CheckpointRefManifest, CheckpointSequenceRange, CheckpointSourceKind,
    CheckpointValidationPolicy, CitationBackedCheckpoint, CitationCompactionPolicy,
    CompactedCheckpoint, CompactedCheckpointCandidate, ContextCompiler, ContextEvidence,
    ContextSummary, LedgerFactKind, LedgerProjection, ProjectRules, RegisteredTool, Runtime,
    RuntimeModelRole, SkillCatalog, SkillMetadata, StepContext, StepInput, TaskAnchor,
    TokioProcessRunner, ToolActionKind, ToolExecutionContext, ToolExecutionError,
    ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture, citation_compaction_response_schema,
    citation_compaction_system_prompt, process_command_tool,
};
use schemars::Schema;
use serde_json::{Map, Value, json};
use std::{
    env,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[path = "provider_boundary/diagnostics.rs"]
mod diagnostics;

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("valid artifact id")
}

fn citation_checkpoint_for_provider_tests(
    checkpoint_id: &str,
    ref_id: &str,
    excerpt: &str,
) -> CompactedCheckpoint {
    let manifest = CheckpointRefManifest::new(
        CheckpointId::new(checkpoint_id).expect("valid checkpoint id"),
        vec![
            CheckpointRef::new(
                CheckpointRefId::new(ref_id).expect("valid ref id"),
                CheckpointSourceKind::UserMessage,
                "history:1",
                CheckpointSequenceRange::new(1, 1).expect("valid range"),
                "body[0]",
                excerpt,
            )
            .expect("valid ref"),
        ],
    )
    .expect("valid manifest");
    let candidate = CompactedCheckpointCandidate::from_json(&format!(
        r#"{{
          "claims": [
            {{
              "id": "c1",
              "kind": "constraint",
              "text": {excerpt_json},
              "refs": [{ref_json}]
            }}
          ],
          "working_intent": null
        }}"#,
        excerpt_json = serde_json::to_string(excerpt).expect("excerpt serializes"),
        ref_json = serde_json::to_string(ref_id).expect("ref id serializes"),
    ))
    .expect("candidate parses");
    let citation = CitationBackedCheckpoint::from_candidate(
        CheckpointId::new(checkpoint_id).expect("valid checkpoint id"),
        candidate,
        manifest,
        CheckpointValidationPolicy::default(),
    )
    .expect("citation checkpoint builds");

    CompactedCheckpoint::from_citation_backed(citation).expect("checkpoint renders")
}

fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

fn completed_event() -> ModelEvent {
    completed_event_with_finish(FinishReason::Stop)
}

fn completed_event_with_finish(finish_reason: FinishReason) -> ModelEvent {
    completed_outputs_event(vec![ModelOutput::text("model result")], finish_reason)
}

fn completed_text_event(text: &str) -> ModelEvent {
    completed_outputs_event(vec![ModelOutput::text(text)], FinishReason::Stop)
}

fn completed_outputs_event(outputs: Vec<ModelOutput>, finish_reason: FinishReason) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(outputs, finish_reason, None),
    }
}

fn model_tool_call() -> ModelToolCall {
    model_tool_call_with_id("call-1")
}

fn shell_process_tool_call() -> ModelToolCall {
    model_tool_call_with_args(
        "call-real-shell",
        "run_process",
        Map::from_iter([
            (
                "argv".to_owned(),
                json!(["bash", "-lc", "echo ProcessRunner | wc -l"]),
            ),
            ("cwd".to_owned(), json!(".")),
        ]),
    )
}

fn model_tool_call_with_id(id: &str) -> ModelToolCall {
    model_tool_call_with_args(
        id,
        "search_notes",
        Map::from_iter([("query".to_owned(), json!("test query"))]),
    )
}

fn model_tool_call_with_args(id: &str, name: &str, arguments: Map<String, Value>) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid tool call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::new(arguments),
    )
}

fn runtime_with_provider(session: &str, provider: FakeModelProvider) -> Runtime {
    Runtime::builder(session_id(session))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

fn runtime_with_provider_event_buffer(
    session: &str,
    provider: FakeModelProvider,
    event_buffer_size: usize,
) -> Runtime {
    Runtime::builder(session_id(session))
        .event_buffer_size(NonZeroUsize::new(event_buffer_size).expect("non-zero buffer"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

fn runtime_with_scripted_provider(session: &str, provider: ScriptedModelProvider) -> Runtime {
    Runtime::builder(session_id(session))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

#[derive(Debug, serde::Deserialize)]
struct CitationCompactionFixture {
    messages: Vec<FixtureMessage>,
    candidate: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
struct FixtureMessage {
    role: String,
    text: String,
}

fn live_openai_provider_from_env() -> Option<(OpenAiProvider, ModelName)> {
    if std::env::var("MERRY_OPENAI_LIVE_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping live compactor test: set MERRY_OPENAI_LIVE_TESTS=1");
        return None;
    }

    let api_key = match std::env::var("MERRY_OPENAI_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
    {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping live compactor test: set MERRY_OPENAI_API_KEY or OPENAI_API_KEY");
            return None;
        }
    };
    let model = match std::env::var("MERRY_OPENAI_MODEL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping live compactor test: set MERRY_OPENAI_MODEL");
            return None;
        }
    };
    let mut config = match OpenAiProviderConfig::new(&api_key) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("skipping live compactor test: invalid OpenAI config: {error}");
            return None;
        }
    };
    if let Ok(base_url) = std::env::var("MERRY_OPENAI_BASE_URL") {
        config = match config.with_base_url(&base_url) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("skipping live compactor test: invalid MERRY_OPENAI_BASE_URL: {error}");
                return None;
            }
        };
    }
    let model = match ModelName::new(&model) {
        Ok(model) => model,
        Err(error) => {
            eprintln!("skipping live compactor test: invalid MERRY_OPENAI_MODEL: {error}");
            return None;
        }
    };

    Some((OpenAiProvider::new(config), model))
}

#[derive(Debug, Clone)]
struct ScriptedModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Arc<Mutex<Vec<ScriptedProviderStep>>>,
    recorded_requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[derive(Debug)]
enum ScriptedProviderStep {
    Stream(Vec<Result<ModelEvent, ModelError>>),
    SetupError(ModelError),
}

impl ScriptedModelProvider {
    fn new(scripts: Vec<Vec<Result<ModelEvent, ModelError>>>) -> Self {
        Self::new_steps(
            scripts
                .into_iter()
                .map(ScriptedProviderStep::Stream)
                .collect(),
        )
    }

    fn new_steps(steps: Vec<ScriptedProviderStep>) -> Self {
        Self {
            name: ProviderName::new("scripted-model-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            steps: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded_requests(&self) -> Vec<ModelRequest> {
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
        request: ModelRequest,
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

            let step = self
                .steps
                .lock()
                .expect("steps mutex should not be poisoned")
                .pop()
                .unwrap_or_else(|| ScriptedProviderStep::Stream(Vec::new()));

            match step {
                ScriptedProviderStep::Stream(script) => {
                    let event_stream: ModelEventStream = Box::pin(stream::iter(script));
                    Ok(event_stream)
                }
                ScriptedProviderStep::SetupError(error) => Err(error),
            }
        })
    }
}

async fn collect_step(runtime: &Runtime, text: &str) -> Vec<RuntimeJournalEvent> {
    collect_step_with_context(runtime, text, StepContext::new(CancellationToken::new())).await
}

async fn collect_step_with_context(
    runtime: &Runtime,
    text: &str,
    context: StepContext,
) -> Vec<RuntimeJournalEvent> {
    runtime
        .step(
            StepInput::user_text(text).expect("valid step input"),
            context,
        )
        .expect("step should start")
        .collect()
        .await
}

async fn seed_history_text_for_compaction(runtime: &Runtime, old_user: &str, retained_tail: &str) {
    let first_events = collect_step(runtime, old_user).await;
    assert!(
        first_events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "first seed step should complete"
    );
    let second_events = collect_step(runtime, retained_tail).await;
    assert!(
        second_events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "tail seed step should complete"
    );
}

fn fixture_provider_steps(
    fixture: &CitationCompactionFixture,
) -> Vec<Vec<Result<ModelEvent, ModelError>>> {
    let mut assistant_texts = fixture
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| message.text.as_str());
    let user_count = fixture
        .messages
        .iter()
        .filter(|message| message.role == "user")
        .count();
    let mut steps = Vec::with_capacity(user_count + 1);
    for _ in 0..user_count {
        let text = assistant_texts.next().unwrap_or("fixture assistant ack");
        steps.push(vec![Ok(completed_text_event(text))]);
    }
    steps.push(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text(
            &serde_json::to_string(&fixture.candidate).expect("candidate serializes"),
        )],
        FinishReason::Stop,
    ))]);
    steps
}

async fn seed_fixture_messages(runtime: &Runtime, messages: &[FixtureMessage]) {
    for message in messages {
        match message.role.as_str() {
            "user" => {
                let events = collect_step(runtime, &message.text).await;
                assert!(
                    events
                        .iter()
                        .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
                    "fixture user message should complete"
                );
            }
            "assistant" => {}
            other => panic!("unsupported fixture role: {other}"),
        }
    }
}

fn messy_live_compaction_user_messages() -> Vec<String> {
    vec![
        concat!(
            "We need compaction for long sessions, but the conversation keeps looping. ",
            "A previous assistant suggested just keeping the full chat because it feels safer. ",
            "That is not acceptable: raw context will eventually exceed the window, and the ",
            "continuation should survive without replaying every correction and false start. ",
            "Please remember that this is a runtime compaction problem, not a branding problem."
        )
        .to_owned(),
        concat!(
            "Here is a noisy correction. The artifact graph idea sounds attractive because it ",
            "can attach evidence to everything, but it is not the current context-growth solution. ",
            "For this slice, artifacts are exact reads and tool outputs; they are not a permanent ",
            "semantic evidence graph. If the summary revives artifact graph as the main solution, ",
            "the compactor has failed the design conversation."
        )
        .to_owned(),
        concat!(
            "Another detour: maybe runtime rules can decide what is important. That is wrong in ",
            "the open-ended semantic case. Runtime can validate shape, refs, pending-call boundaries, ",
            "byte budgets, and deterministic rendering. Runtime cannot know whether a nuanced design ",
            "claim is semantically true. The LLM must summarize, and runtime must treat that output as ",
            "a checkpoint candidate, not as proven truth."
        )
        .to_owned(),
        concat!(
            "Function-call continuity is especially annoying. Tool call and tool result pairs must ",
            "remain provider-visible until a checkpoint replaces the covered window. Do not summarize ",
            "a tool result immediately just because an artifact exists. The provider still needs protocol ",
            "continuity until compaction. Once compaction succeeds, covered continuations may disappear ",
            "from the next request and the checkpoint should carry only the needed claim with refs."
        )
        .to_owned(),
        concat!(
            "There was also a long argument about pins, modes, resource indexes, repository timelines, ",
            "and tree-sitter scans. They may be future tools, but they are not the core short-term solution. ",
            "The runtime has no intelligence; a pin suggestion might help a human, but automatic pinning ",
            "would either be too rigid to generalize or too loose to mean anything. Keep this as a rejected ",
            "or non-current path when it matters."
        )
        .to_owned(),
        concat!(
            "The retained raw tail must stay outside the compactor input. This is important because the ",
            "current user message and recent tail will still be projected raw after the checkpoint. If the ",
            "compactor summarizes the tail, the next prompt will duplicate it and the model may confuse old ",
            "working intent with the actual current user message. Working intent, when present, belongs near ",
            "the checkpoint footer, subordinate to task anchor and raw user text."
        )
        .to_owned(),
        concat!(
            "Messy filler with useful signal hidden inside: earlier we worried that citation refs might create ",
            "a permanent database, then decided refs are local to the installed checkpoint. They should answer ",
            "why a claim exists and allow bounded source inspection. They do not prove semantic truth. They should ",
            "not force the runtime to keep every old prompt body forever. Rolling compaction may cite prior checkpoint ",
            "claims with capped lineage."
        )
        .to_owned(),
        concat!(
            "More noise: if this were a coding agent over a git repo, exact file reads may be cheaper than storing ",
            "file artifacts forever. If this were a general local agent with no stable files, artifacts may matter more. ",
            "Do not let this distract the checkpoint. The current checkpoint should capture the execution and evidence ",
            "context needed for continuation, and exact evidence should be re-read only when needed."
        )
        .to_owned(),
        concat!(
            "A deliberately rambling implementation note: someone says to add a reducer, someone else says that sounds ",
            "like fake engineering, then the discussion circles back to the fact that the runtime can normalize tool events ",
            "but cannot decide open semantic importance. There are examples about a repository task, a general assistant, ",
            "a Python business workflow, and a game helper. Most of those examples are not decisions; they are pressure tests. ",
            "The useful durable point is that checkpoint compaction should keep the current task direction small, inspectable, ",
            "and subordinate to exact evidence reads. It should not transform every observed event into prompt text."
        )
        .to_owned(),
        concat!(
            "Another noisy paragraph about user decisions: durable decisions are hard unless a user explicitly confirms them. ",
            "A model can suggest that a correction looks like a decision, but the runtime should not invent an authoritative ",
            "user decision from ordinary conversation. In this checkpoint smoke, keep user corrections as cited constraints or ",
            "rejected paths, not as a separate decision database. If a future slash command confirms a decision, that can be a ",
            "different feature. For now, do not pretend the compactor has authority it does not have."
        )
        .to_owned(),
        concat!(
            "Yet more mixed context: there was confusion about stable prefix versus dynamic control context. Stable prefix is ",
            "for cacheable instructions and tool descriptions. Task anchor, checkpoint, transcript body, tool exchanges, ",
            "and current user input are dynamic. Working intent is not a new top-level stable segment; it is checkpoint-adjacent ",
            "footer content and must not compete with the current user message. This should survive the summary because it affects ",
            "prompt assembly and prevents duplicated or stale intent."
        )
        .to_owned(),
        concat!(
            "A low-value but realistic tangent says maybe tree-sitter repository scans, resource keys, or file timelines could ",
            "provide a compass. The conclusion was not to build that now. For git repositories, exact reads and tests often beat ",
            "storing huge stale artifacts. For non-file agents, some durable storage may matter later. This is context, not the ",
            "current implementation target. The checkpoint should compress it into a short non-current path if it matters, or drop ",
            "it if it does not affect continuation."
        )
        .to_owned(),
        concat!(
            "There is also frustration in the conversation: summaries can hallucinate, previous assistant replies can be wrong, ",
            "and model-generated plans can drift. A citation-backed checkpoint does not magically solve truth. It gives later turns ",
            "a way to inspect why a claim entered the checkpoint. Claims with no ref should be rejected. Claims that cite irrelevant ",
            "refs are still possible and must be evaluated in live smoke. That is exactly why this test wants a messy live compactor ",
            "run, not only a scripted candidate."
        )
        .to_owned(),
        concat!(
            "One more process note before the actual tail: the live compactor quality test should call only the compaction model. ",
            "It should not run a shell, patch files, or require bubblewrap. The seed history can use a scripted primary provider, ",
            "while RuntimeModelRole::ContextCompaction points at the real OpenAI-compatible provider. The output should be printed ",
            "with --nocapture so a human can inspect whether the summary preserved constraints, rejected paths, and local refs."
        )
        .to_owned(),
        concat!(
            "The final direction for this live compactor smoke: produce citation-backed checkpoint claims with local refs. ",
            "Preserve rejected paths and corrected misunderstandings. Keep claims short. Do not include the retained tail. ",
            "Do not invent completed implementation work. If evidence is ambiguous, write an open question instead of a fake fact."
        )
        .to_owned(),
        concat!(
            "Retained tail sentinel one: this recent note should remain raw after compaction and must not appear in the ",
            "compactor input or the checkpoint candidate. It talks about running live smoke manually and should not be ",
            "compressed into the durable checkpoint."
        )
        .to_owned(),
        concat!(
            "Retained tail sentinel two: the next request after compaction should still see this exact recent tail as raw ",
            "history. If a checkpoint claim mentions this sentinel, the test should be treated as low quality."
        )
        .to_owned(),
    ]
}

fn rough_token_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

async fn collect_text_output(stream: ModelEventStream) -> Result<String, String> {
    let mut stream = stream;
    let mut saw_delta = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(ModelEvent::Started) => {}
            Ok(ModelEvent::OutputTextDelta { delta }) => {
                if !delta.is_empty() {
                    saw_delta = true;
                }
            }
            Ok(ModelEvent::ToolCallRequested { .. }) => {
                return Err("model requested a tool call".to_owned());
            }
            Ok(ModelEvent::Completed { response }) => {
                if response.finish_reason() != FinishReason::Stop {
                    return Err(format!(
                        "model finished with {:?}",
                        response.finish_reason()
                    ));
                }
                let [ModelOutput::Text { text }] = response.outputs() else {
                    return Err("model must return exactly one text output".to_owned());
                };
                if saw_delta {
                    eprintln!("live compactor emitted streaming text deltas");
                }
                return Ok(text.clone());
            }
            Err(error) => return Err(error.to_string()),
        }
    }

    Err("model stream ended before completion".to_owned())
}

fn command_is_available(program: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&path).any(|directory| directory.join(program).is_file())
}

fn event_kind_names(events: &[RuntimeJournalEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event.payload {
            RuntimeJournalPayload::SessionStarted => "SessionStarted",
            RuntimeJournalPayload::StepStarted => "StepStarted",
            RuntimeJournalPayload::SessionUsageUpdated { .. } => "SessionUsageUpdated",
            RuntimeJournalPayload::StepCompleted => "StepCompleted",
            RuntimeJournalPayload::Cancelled { .. } => "Cancelled",
            RuntimeJournalPayload::Failed { .. } => "Failed",
            RuntimeJournalPayload::ArtifactRecorded { .. } => "ArtifactRecorded",
            RuntimeJournalPayload::AssistantOutputRecorded { .. } => "AssistantOutputRecorded",
            RuntimeJournalPayload::EvidenceReferenced { .. } => "EvidenceReferenced",
            RuntimeJournalPayload::ToolCallPending { .. } => "ToolCallPending",
            RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
            RuntimeJournalPayload::SkillUsed { .. } => "SkillUsed",
            _ => "Unknown",
        })
        .collect()
}

fn failed_code(events: &[RuntimeJournalEvent]) -> Option<&str> {
    events.iter().find_map(|event| match &event.payload {
        RuntimeJournalPayload::Failed { diagnostic } => Some(diagnostic.code()),
        _ => None,
    })
}

fn failed_sequence(events: &[RuntimeJournalEvent]) -> u64 {
    events
        .iter()
        .find_map(|event| match event.payload {
            RuntimeJournalPayload::Failed { .. } => Some(event.sequence),
            _ => None,
        })
        .expect("failed event should be present")
}

fn assert_no_completion(events: &[RuntimeJournalEvent]) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "terminal failure/cancellation must not be followed by StepCompleted: {events:?}"
    );
}

fn assert_no_artifact_recorded(events: &[RuntimeJournalEvent]) {
    assert!(
        events.iter().all(|event| !matches!(
            event.payload,
            RuntimeJournalPayload::ArtifactRecorded { .. }
        )),
        "terminal failure/cancellation must not record artifacts: {events:?}"
    );
}

fn assert_no_tool_call_pending(events: &[RuntimeJournalEvent]) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::ToolCallPending { .. })),
        "terminal failure/cancellation must not record pending tool calls: {events:?}"
    );
}

fn assert_no_failed(events: &[RuntimeJournalEvent]) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::Failed { .. })),
        "terminal cancellation must not emit Failed: {events:?}"
    );
}

async fn record_valid_context(runtime: &Runtime) -> String {
    let artifact = ArtifactRef::new(
        artifact_id("provider-boundary-context-artifact"),
        ArtifactKind::Text,
    );
    runtime
        .record_artifact(
            artifact.clone(),
            ArtifactContent::text("alpha\nbeta\ngamma\n"),
        )
        .await
        .expect("artifact should record through eventful path");
    let evidence = runtime
        .evidence_ref(
            artifact.id(),
            EvidenceLocator::line_range(2, 3).expect("valid line range"),
        )
        .await
        .expect("evidence should resolve");

    runtime
        .record_context_summary(
            ContextSummary::new(
                "provider-boundary-summary",
                "Provider boundary context is compiled.",
                vec![
                    ContextEvidence::new("selected lines", evidence)
                        .expect("valid context evidence"),
                ],
            )
            .expect("valid context summary"),
        )
        .await
        .expect("context summary should record");

    ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("context should compile")
        .to_snapshot()
}

fn assistant_output_artifact(events: &[RuntimeJournalEvent]) -> &ArtifactRef {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::AssistantOutputRecorded { artifact } => Some(artifact),
            _ => None,
        })
        .expect("assistant output artifact should be recorded")
}

fn pending_tool_call(events: &[RuntimeJournalEvent]) -> &PendingToolCall {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => Some(call),
            _ => None,
        })
        .expect("pending tool call should be emitted")
}

fn failed_tool_result(
    call_id: ToolCallId,
    artifact: ArtifactRef,
    code: &str,
    message: &str,
) -> ToolCallResult {
    ToolCallResult::failed(
        call_id,
        artifact,
        merry_core::ErrorInfo::new(code, message).expect("valid diagnostic"),
    )
}

fn test_tool_spec(name: &str) -> ToolSpec {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" }
        },
        "required": ["query"]
    }))
    .expect("test schema should be a JSON schema");

    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Search test notes",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

fn assert_default_checkpoint_ref_tool(tools: &[ToolSpec]) {
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name().as_str(), "merry_read_checkpoint_ref");
}

fn assert_tools_are_default_checkpoint_ref_plus(
    tools: &[ToolSpec],
    expected_user_tools: &[ToolSpec],
) {
    assert_eq!(tools.len(), expected_user_tools.len() + 1);
    assert_eq!(tools[0].name().as_str(), "merry_read_checkpoint_ref");
    assert_eq!(&tools[1..], expected_user_tools);
}

fn path_tool_spec(name: &str) -> ToolSpec {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" }
        },
        "required": ["path"],
        "additionalProperties": false
    }))
    .expect("test schema should be a JSON schema");

    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Read a workspace file",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

#[derive(Clone)]
struct ScriptedToolExecutor {
    calls: Arc<Mutex<Vec<PendingToolCall>>>,
    response: ToolExecutorResponse,
}

#[derive(Clone)]
enum ToolExecutorResponse {
    Outcome(ToolExecutionOutcome),
    Error(Arc<dyn Fn() -> ToolExecutionError + Send + Sync>),
}

impl ScriptedToolExecutor {
    fn succeeding_text(text: &str) -> Self {
        Self::new(ToolExecutorResponse::Outcome(
            ToolExecutionOutcome::succeeded_text(text),
        ))
    }

    fn failing_json(code: &str, content: &str) -> Self {
        Self::new(ToolExecutorResponse::Outcome(
            ToolExecutionOutcome::failed_json(
                content,
                ErrorInfo::new(code, "tool domain failure").expect("valid diagnostic"),
            ),
        ))
    }

    fn infrastructure_error(message: &str) -> Self {
        let message = message.to_owned();
        Self::new(ToolExecutorResponse::Error(Arc::new(move || {
            ToolExecutionError::infrastructure(message.clone())
        })))
    }

    fn new(response: ToolExecutorResponse) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response,
        }
    }

    fn calls(&self) -> Vec<PendingToolCall> {
        self.calls
            .lock()
            .expect("tool calls mutex should not be poisoned")
            .clone()
    }
}

impl ToolExecutor for ScriptedToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("tool calls mutex should not be poisoned")
                .push(call);

            match &self.response {
                ToolExecutorResponse::Outcome(outcome) => Ok(outcome.clone()),
                ToolExecutorResponse::Error(error) => Err(error()),
            }
        })
    }
}

#[derive(Clone)]
struct ReentrantMutationExecutor {
    runtime: Arc<Mutex<Option<Runtime>>>,
    observations: Arc<Mutex<Vec<ReentrantMutationObservation>>>,
}

impl ReentrantMutationExecutor {
    fn new() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(None)),
            observations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn set_runtime(&self, runtime: Runtime) {
        *self
            .runtime
            .lock()
            .expect("reentrant runtime mutex should not be poisoned") = Some(runtime);
    }

    fn observations(&self) -> Vec<ReentrantMutationObservation> {
        self.observations
            .lock()
            .expect("reentrant observations mutex should not be poisoned")
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReentrantMutationObservation {
    RecordStepAlreadyActive,
    SubmitStepAlreadyActive,
    Other(String),
}

impl ToolExecutor for ReentrantMutationExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let runtime = self
                .runtime
                .lock()
                .expect("reentrant runtime mutex should not be poisoned")
                .clone()
                .expect("runtime should be installed before executor runs");
            let artifact =
                ArtifactRef::new(artifact_id("executor-inner-artifact"), ArtifactKind::Text);
            let record_observation = match runtime
                .record_artifact(
                    artifact.clone(),
                    ArtifactContent::text("inner artifact must not record\n"),
                )
                .await
            {
                Ok(_) => ReentrantMutationObservation::Other(
                    "record_artifact unexpectedly succeeded".to_owned(),
                ),
                Err(merry_runtime::RuntimeError::StepAlreadyActive { .. }) => {
                    ReentrantMutationObservation::RecordStepAlreadyActive
                }
                Err(error) => ReentrantMutationObservation::Other(error.to_string()),
            };
            self.observations
                .lock()
                .expect("reentrant observations mutex should not be poisoned")
                .push(record_observation);

            let result = ToolCallResult::succeeded(call.id().clone(), artifact);
            let submit_observation = match runtime
                .submit_tool_result(
                    result,
                    ArtifactContent::text("inner result must not resolve\n"),
                )
                .await
            {
                Ok(_) => ReentrantMutationObservation::Other(
                    "submit_tool_result unexpectedly succeeded".to_owned(),
                ),
                Err(merry_runtime::RuntimeError::StepAlreadyActive { .. }) => {
                    ReentrantMutationObservation::SubmitStepAlreadyActive
                }
                Err(error) => ReentrantMutationObservation::Other(error.to_string()),
            };
            self.observations
                .lock()
                .expect("reentrant observations mutex should not be poisoned")
                .push(submit_observation);

            Ok(ToolExecutionOutcome::succeeded_text(
                "outer executor result\n",
            ))
        })
    }
}

fn runtime_with_registered_tool(
    session: &str,
    provider: ScriptedModelProvider,
    executor: ScriptedToolExecutor,
) -> Runtime {
    Runtime::builder(session_id(session))
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("search_notes"),
            Arc::new(executor),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

fn runtime_with_registered_tool_action(
    session: &str,
    provider: ScriptedModelProvider,
    executor: ScriptedToolExecutor,
    action_kind: ToolActionKind,
) -> Runtime {
    Runtime::builder(session_id(session))
        .register_tool(RegisteredTool::new(
            test_tool_spec("search_notes"),
            Arc::new(executor),
            action_kind,
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

fn resolved_tool_result(events: &[RuntimeJournalEvent]) -> &ToolCallResult {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("resolved tool call should be emitted")
}

fn assert_sanitized_policy_denial_json(value: &Value, tool_name: &str) {
    assert_eq!(
        value,
        &json!({
            "ok": false,
            "tool": tool_name,
            "error": {
                "code": "action_policy_denied",
                "message": "tool action was blocked by runtime policy"
            }
        })
    );
    assert!(value.get("call_id").is_none());
    assert!(value.get("action_kind").is_none());
    assert!(value.get("policy").is_none());
    assert!(value.get("reason").is_none());
    assert!(value.get("provider").is_none());
    assert!(value.get("provider_response").is_none());
    assert!(value.get("wire").is_none());
    assert!(value.get("previous_response_id").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_with_provider_compiles_user_text_request_and_records_assistant_output_artifact()
 {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::OutputTextDelta {
            delta: "ignored".to_owned(),
        }),
        Ok(completed_event()),
    ]);
    let runtime = runtime_with_provider("provider-user-text", provider.clone());

    let events = collect_step(&runtime, "Explain the runtime boundary.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    let artifact = assistant_output_artifact(&events);
    assert_eq!(artifact.id().as_str(), "assistant-output-2");
    assert_eq!(artifact.kind(), &ArtifactKind::Text);
    let evidence = runtime
        .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("artifact event should be observable only after artifact is readable");
    assert_eq!(evidence.artifact_id, *artifact.id());

    let projection = runtime.ledger_projection().await;
    assert_eq!(
        projection.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::StepCompleted,
            },
        ]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.model(), &model_name());
    assert_eq!(request.messages().len(), 2);
    assert_eq!(request.stable_prefix_message_count(), 1);
    assert_eq!(request.messages()[0].role(), ModelMessageRole::System);
    assert!(
        request.messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a pragmatic coding agent.")
    );
    assert!(
        !request.messages()[0]
            .content()
            .as_text()
            .contains("brief progress note first"),
        "plain runtime requests must not induce tool-progress commentary by default"
    );
    assert_eq!(request.messages()[1].role(), ModelMessageRole::User);
    assert_eq!(
        request.messages()[1].content().as_text(),
        "Explain the runtime boundary."
    );
    assert_default_checkpoint_ref_tool(request.tools());
    assert_eq!(request.generation(), &GenerationConfig::default());
    assert!(!request.generation().allow_parallel_tool_calls());
}

#[tokio::test(flavor = "current_thread")]
async fn reserved_assistant_output_external_recording_does_not_block_runtime_owned_output() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-reserved-assistant-output", provider);
    let artifact = ArtifactRef::new(artifact_id("assistant-output-3"), ArtifactKind::Text);
    let before = runtime.ledger_projection().await;

    let err = runtime
        .record_artifact(
            artifact.clone(),
            ArtifactContent::text("external shadow output\n"),
        )
        .await
        .expect_err("external recording should not use runtime-owned assistant output ids");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::ReservedArtifactId { artifact_id } if artifact_id == *artifact.id()
    ));
    assert_eq!(before, after);
    let evidence_err = runtime
        .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("reserved artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == *artifact.id()
    ));

    let events = collect_step(&runtime, "after reserved artifact").await;
    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    let generated = assistant_output_artifact(&events);
    assert_eq!(generated.id().as_str(), "assistant-output-2");
    let evidence = runtime
        .evidence_ref(generated.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("runtime-owned assistant output should be readable");
    assert_eq!(evidence.artifact_id, *generated.id());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_with_provider_uses_step_generation_config() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-generation-config", provider.clone());
    let context = StepContext::new(CancellationToken::new()).with_generation_config(
        GenerationConfig::new(Some(16), false).expect("valid generation config"),
    );

    let events = collect_step_with_context(&runtime, "Limit the output.", context).await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].generation().max_output_tokens(), Some(16));
    assert!(!requests[0].generation().allow_parallel_tool_calls());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_with_provider_includes_compiled_context_as_system_message() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-context", provider.clone());
    let expected_snapshot = record_valid_context(&runtime).await;

    let events = collect_step(&runtime, "Use the stored context.").await;

    assert_eq!(
        event_kind_names(&events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.messages().len(), 3);
    assert_eq!(request.stable_prefix_message_count(), 1);
    assert_eq!(request.messages()[0].role(), ModelMessageRole::System);
    assert!(
        request.messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a pragmatic coding agent.")
    );
    assert_eq!(request.messages()[1].role(), ModelMessageRole::System);
    assert_eq!(request.messages()[1].content().as_text(), expected_snapshot);
    assert_eq!(request.messages()[2].role(), ModelMessageRole::User);
    assert_eq!(
        request.messages()[2].content().as_text(),
        "Use the stored context."
    );
    assert_default_checkpoint_ref_tool(request.tools());
    assert!(!request.generation().allow_parallel_tool_calls());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_multiline_text_artifact_supports_line_evidence() {
    let provider = FakeModelProvider::new(vec![Ok(completed_text_event(
        "first line\nsecond line\nthird line\n",
    ))]);
    let runtime = runtime_with_provider("provider-multiline-output", provider);

    let events = collect_step(&runtime, "Return multiple lines.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    let artifact = assistant_output_artifact(&events);
    let evidence = runtime
        .evidence_ref(
            artifact.id(),
            EvidenceLocator::line_range(2, 2).expect("valid line range"),
        )
        .await
        .expect("assistant output line evidence should resolve");
    assert_eq!(evidence.artifact_id, *artifact.id());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_success_with_single_slot_event_buffer_emits_artifact_and_completion() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider_event_buffer("provider-stop-buffer-one", provider, 1);

    let events = collect_step(&runtime, "Use a single-slot event buffer.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn second_provider_step_continues_sequences_and_replays_transcript() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-second-step", provider.clone());

    let first_events = collect_step(&runtime, "First request.").await;
    let second_events = collect_step(&runtime, "Second request.").await;

    assert_eq!(
        first_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        second_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5, 6]
    );
    assert_eq!(
        event_kind_names(&second_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert_eq!(
        assistant_output_artifact(&first_events).id().as_str(),
        "assistant-output-2"
    );
    assert_eq!(
        assistant_output_artifact(&second_events).id().as_str(),
        "assistant-output-5"
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages().len(), 4);
    assert_eq!(requests[1].messages()[0].role(), ModelMessageRole::System);
    assert!(
        requests[1].messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a pragmatic coding agent.")
    );
    assert_eq!(requests[1].messages()[1].role(), ModelMessageRole::User);
    assert_eq!(
        requests[1].messages()[1].content().as_text(),
        "First request."
    );
    assert_eq!(
        requests[1].messages()[2].role(),
        ModelMessageRole::Assistant
    );
    assert_eq!(
        requests[1].messages()[2].content().as_text(),
        "model result"
    );
    assert_eq!(requests[1].messages()[3].role(), ModelMessageRole::User);
    assert_eq!(
        requests[1].messages()[3].content().as_text(),
        "Second request."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_with_provider_emits_failed_when_context_compile_fails() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-context-failure", provider.clone());
    runtime
        .record_context_summary(
            ContextSummary::new(
                "invalid-summary",
                "This summary has no evidence.",
                Vec::new(),
            )
            .expect("summary construction allows compiler validation"),
        )
        .await
        .expect("raw context summary should record");

    let events = collect_step(&runtime, "Compile context.").await;

    assert_eq!(provider.recorded_requests().len(), 0);
    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("context_compile"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stream_error_emits_failed_without_step_completed() {
    let provider = FakeModelProvider::new(vec![Err(merry_llm::ModelError::provider(
        ProviderErrorKind::Protocol,
        "bad\u{7}provider\nmessage",
    ))]);
    let runtime = runtime_with_provider("provider-stream-error", provider);

    let events = collect_step(&runtime, "Stream failure.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_protocol"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);

    let failed_sequence = failed_sequence(&events);
    let projection = runtime.ledger_projection().await;
    assert!(projection.entries().contains(&LedgerProjection::Lifecycle {
        sequence: failed_sequence,
        order: failed_sequence,
        kind: LedgerFactKind::Failed
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stream_eof_before_completed_emits_failed() {
    let provider = FakeModelProvider::new(vec![Ok(ModelEvent::Started)]);
    let runtime = runtime_with_provider("provider-eof", provider);

    let events = collect_step(&runtime, "EOF before completion.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_stream_eof"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_tool_call_requested_emits_pending_without_completion() {
    let call = model_tool_call();
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested { call: call.clone() }),
        Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-streamed", provider);

    let events = collect_step(&runtime, "Request a tool.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(pending_tool_call(&events).id().as_str(), "call-1");
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
    assert!(failed_code(&events).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn registered_tool_specs_are_compiled_into_provider_request() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let tool = test_tool_spec("search_notes");
    let runtime = Runtime::builder(session_id("provider-registered-tool-spec"))
        .register_tool(RegisteredTool::read_only(
            tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "Use registered tools.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_tools_are_default_checkpoint_ref_plus(requests[0].tools(), &[tool]);
    assert!(
        requests[0]
            .tool_profile_hash()
            .as_str()
            .starts_with("fnv1a64:")
    );
    assert!(requests[0].continuations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_request_keeps_tool_exchange_before_later_user_turn() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-read"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-transcript-order", provider.clone());

    let first_events = collect_step(&runtime, "Read the file.").await;
    let pending = pending_tool_call(&first_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                pending.id().clone(),
                ArtifactRef::new(
                    artifact_id("provider-transcript-result"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("file contents\n"),
        )
        .await
        .expect("tool result should resolve");

    collect_step(&runtime, "Now answer a new request.").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let dynamic = requests[1].dynamic_input();
    assert!(matches!(dynamic[0], ModelInputItem::Message(_)));
    assert!(matches!(dynamic[1], ModelInputItem::ToolCall(_)));
    assert!(matches!(dynamic[2], ModelInputItem::ToolResult(_)));
    assert!(matches!(dynamic[3], ModelInputItem::Message(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn compiled_provider_request_tool_profile_hash_tracks_registered_tools() {
    let first_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let search_tool = test_tool_spec("search_notes");
    let read_tool = test_tool_spec("read_file");
    let first_runtime = Runtime::builder(session_id("provider-tool-profile-hash-first"))
        .register_tool(RegisteredTool::read_only(
            search_tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .register_tool(RegisteredTool::read_only(
            read_tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(first_provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&first_runtime, "Use registered tools.").await;
    let first_hash = first_provider.recorded_requests()[0]
        .tool_profile_hash()
        .clone();

    let reordered_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let reordered_runtime = Runtime::builder(session_id("provider-tool-profile-hash-reordered"))
        .register_tool(RegisteredTool::read_only(
            read_tool,
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .register_tool(RegisteredTool::read_only(
            search_tool,
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(reordered_provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&reordered_runtime, "Use registered tools.").await;
    assert_eq!(
        reordered_provider.recorded_requests()[0].tool_profile_hash(),
        &first_hash
    );

    let changed_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let changed_runtime = Runtime::builder(session_id("provider-tool-profile-hash-changed"))
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("read_file"),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(changed_provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&changed_runtime, "Use registered tools.").await;
    assert_ne!(
        changed_provider.recorded_requests()[0].tool_profile_hash(),
        &first_hash
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compiled_provider_request_stable_prefix_hash_tracks_base_instructions_and_tools_only() {
    let first_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let read_tool = test_tool_spec("read_file");
    let first_runtime = Runtime::builder(session_id("provider-stable-prefix-first"))
        .register_tool(RegisteredTool::read_only(
            read_tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(first_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&first_runtime, "First dynamic request.").await;
    let first_request = first_provider.recorded_requests()[0].clone();

    let dynamic_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let dynamic_runtime = Runtime::builder(session_id("provider-stable-prefix-dynamic"))
        .register_tool(RegisteredTool::read_only(
            read_tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(dynamic_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    let _snapshot = record_valid_context(&dynamic_runtime).await;
    collect_step(&dynamic_runtime, "Second dynamic request.").await;
    let dynamic_request = dynamic_provider.recorded_requests()[0].clone();

    assert_eq!(
        first_request.stable_prefix_hash(),
        dynamic_request.stable_prefix_hash()
    );
    assert_ne!(
        first_request.dynamic_context_hash(),
        dynamic_request.dynamic_context_hash()
    );

    let changed_tools_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let changed_tools_runtime = Runtime::builder(session_id("provider-stable-prefix-tool-change"))
        .model_provider(Arc::new(changed_tools_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&changed_tools_runtime, "First dynamic request.").await;
    assert_ne!(
        first_request.stable_prefix_hash(),
        changed_tools_provider.recorded_requests()[0].stable_prefix_hash()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn project_rules_enter_stable_prefix_and_affect_stable_hash() {
    let first_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let first_runtime = Runtime::builder(session_id("provider-project-rules-first"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use fixture rule A.\n").expect("valid project rules"),
        )
        .model_provider(Arc::new(first_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&first_runtime, "Inspect project.").await;
    let first_request = first_provider.recorded_requests()[0].clone();

    let changed_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let changed_runtime = Runtime::builder(session_id("provider-project-rules-changed"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use fixture rule B.\n").expect("valid project rules"),
        )
        .model_provider(Arc::new(changed_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&changed_runtime, "Inspect project.").await;
    let changed_request = changed_provider.recorded_requests()[0].clone();

    assert_eq!(first_request.stable_prefix_message_count(), 2);
    assert_eq!(first_request.stable_prefix_messages().len(), 2);
    assert_eq!(
        first_request.stable_prefix_messages()[1].role(),
        ModelMessageRole::System
    );
    assert!(
        first_request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("project-rules-source:AGENTS.md")
    );
    assert!(
        first_request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("Use fixture rule A.")
    );
    assert_ne!(
        first_request.stable_prefix_hash(),
        changed_request.stable_prefix_hash()
    );
    assert_eq!(
        first_request.dynamic_context_hash(),
        changed_request.dynamic_context_hash(),
        "same user input with changed project rules should keep dynamic body unchanged"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compiled_provider_request_skill_metadata_enters_stable_prefix_before_project_rules() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let skill_catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "frontend-design",
            "Use when building polished frontend UI.",
            PathBuf::from("skills/frontend-design/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect("valid skill metadata"),
    ])
    .expect("valid skill catalog");
    let runtime = Runtime::builder(session_id("provider-skill-prefix"))
        .skill_catalog(skill_catalog)
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use project rules sentinel.\n")
                .expect("valid project rules"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "Use the skill list.").await;
    let request = provider.recorded_requests()[0].clone();

    assert_eq!(request.stable_prefix_message_count(), 3);
    assert_eq!(request.stable_prefix_messages().len(), 3);
    assert!(
        request.stable_prefix_messages()[0]
            .content()
            .as_text()
            .contains("You are Merry")
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("## Skills")
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("workspace_read_file")
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("skills/frontend-design/SKILL.md")
    );
    assert!(
        !request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("full skill body sentinel")
    );
    assert!(
        request.stable_prefix_messages()[2]
            .content()
            .as_text()
            .contains("project-rules-source:AGENTS.md")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_exposes_skill_metadata_for_completion() {
    let catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "brainstorming",
            "Use for design discussion.",
            PathBuf::from("skills/brainstorming/SKILL.md"),
            PathBuf::from("skills"),
        )
        .expect("valid skill"),
    ])
    .expect("valid catalog");

    let runtime = Runtime::builder(session_id("runtime-skill-list"))
        .skill_catalog(catalog)
        .build()
        .expect("runtime builds");

    let skills = runtime.skills().await;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name(), "brainstorming");

    let found = runtime
        .find_skill("brainstorming")
        .await
        .expect("skill found");
    assert_eq!(found.description(), "Use for design discussion.");
}

#[tokio::test(flavor = "current_thread")]
async fn skill_metadata_changes_stable_prefix_but_not_dynamic_context() {
    let first_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let first_catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "frontend-design",
            "Use for UI work.",
            PathBuf::from("skills/frontend-design/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect("valid skill metadata"),
    ])
    .expect("valid catalog");
    let first_runtime = Runtime::builder(session_id("provider-skill-hash-first"))
        .skill_catalog(first_catalog)
        .model_provider(Arc::new(first_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&first_runtime, "Same dynamic input.").await;
    let first_request = first_provider.recorded_requests()[0].clone();

    let changed_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let changed_catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "frontend-design",
            "Use for UI and responsive layout work.",
            PathBuf::from("skills/frontend-design/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect("valid skill metadata"),
    ])
    .expect("valid catalog");
    let changed_runtime = Runtime::builder(session_id("provider-skill-hash-changed"))
        .skill_catalog(changed_catalog)
        .model_provider(Arc::new(changed_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&changed_runtime, "Same dynamic input.").await;
    let changed_request = changed_provider.recorded_requests()[0].clone();

    assert_ne!(
        first_request.stable_prefix_hash(),
        changed_request.stable_prefix_hash()
    );
    assert_eq!(
        first_request.dynamic_context_hash(),
        changed_request.dynamic_context_hash()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ledger_and_artifact_changes_do_not_change_project_rules_stable_hash() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_event())],
    ]);
    let runtime = Runtime::builder(session_id("provider-project-rules-dynamic-state"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use stable project rules.\n")
                .expect("valid project rules"),
        )
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("search_notes"),
            Arc::new(ScriptedToolExecutor::succeeding_text(
                "dynamic artifact payload\n",
            )),
        ))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Request tool result.").await;
    let first_hash = provider.recorded_requests()[0].stable_prefix_hash().clone();
    let pending = pending_tool_call(&pending_events).clone();
    runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");
    collect_step(&runtime, "Use the resolved tool result.").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].stable_prefix_message_count(), 2);
    assert_eq!(requests[1].stable_prefix_message_count(), 2);
    assert_eq!(requests[1].stable_prefix_hash(), &first_hash);
    assert_ne!(
        requests[0].dynamic_context_hash(),
        requests[1].dynamic_context_hash(),
        "tool exchange and transcript body remain dynamic"
    );
    assert!(
        requests[1]
            .stable_prefix_messages()
            .iter()
            .all(|message| !message
                .content()
                .as_text()
                .contains("dynamic artifact payload")),
        "ledger/artifact changes must not alter project-rules stable prefix"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn empty_checkpoint_slot_renders_no_prompt_text() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = Runtime::builder(session_id("provider-empty-checkpoint-slot"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "No saved state yet.").await;

    let request = provider.recorded_requests()[0].clone();
    assert_eq!(request.messages().len(), 2);
    assert_eq!(request.stable_prefix_message_count(), 1);
    assert!(
        request
            .messages()
            .iter()
            .all(|message| !message.content().as_text().contains("checkpoint:")),
        "empty checkpoint segment must not render prompt text"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_reads_checkpoint_ref_by_checkpoint_and_ref_id() {
    let checkpoint = citation_checkpoint_for_provider_tests(
        "checkpoint-lookup",
        "r1",
        "user rejected resource timelines for this slice",
    );
    let runtime = Runtime::builder(session_id("checkpoint-ref-lookup"))
        .compacted_checkpoint(checkpoint)
        .build()
        .expect("runtime should build");

    let excerpt = runtime
        .read_checkpoint_ref(
            &CheckpointId::new("checkpoint-lookup").expect("valid checkpoint id"),
            &CheckpointRefId::new("r1").expect("valid ref id"),
        )
        .await
        .expect("ref should resolve");

    assert_eq!(
        excerpt.excerpt(),
        "user rejected resource timelines for this slice"
    );
    assert_eq!(excerpt.source_kind(), CheckpointSourceKind::UserMessage);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_compaction_input_excludes_retained_raw_tail() {
    let provider = FakeModelProvider::new(vec![
        Ok(completed_text_event("old assistant message to compact")),
        Ok(completed_text_event("retained raw tail assistant sentinel")),
    ]);
    let runtime = Runtime::builder(session_id("runtime-compaction-input-tail"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "old user message to compact").await;
    collect_step(&runtime, "retained raw tail user sentinel").await;

    let input = runtime
        .citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 2, 1200, 16).expect("valid policy"),
        )
        .await
        .expect("input builds")
        .expect("old prefix should be compressible");
    let payload = input.to_model_payload_json().expect("payload serializes");

    assert!(payload.contains("old user message to compact"));
    assert!(payload.contains("old assistant message to compact"));
    assert!(!payload.contains("retained raw tail user sentinel"));
    assert!(!payload.contains("retained raw tail assistant sentinel"));
}

#[tokio::test(flavor = "current_thread")]
async fn citation_compaction_fixture_preserves_required_design_meanings() {
    let fixture = include_str!("fixtures/citation_compaction_design_fixture.json");
    let fixture: CitationCompactionFixture = serde_json::from_str(fixture).expect("fixture parses");
    let provider = ScriptedModelProvider::new(fixture_provider_steps(&fixture));
    let runtime = Runtime::builder(session_id("citation-fixture"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    seed_fixture_messages(&runtime, &fixture.messages).await;

    let outcome = runtime
        .compact_context_once(
            CitationCompactionPolicy::new(384, None, 8192, 4, 1200, 16).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("compaction succeeds")
        .expect("compaction runs");

    assert!(
        outcome.covered_history_item_count() >= 14,
        "fixture should compact enough history to reveal checkpoint behavior"
    );

    let requests = provider.recorded_requests();
    let compaction_request = requests.last().expect("compaction request exists");
    let compaction_request_text = compaction_request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compaction_request_text.contains("Do not make the artifact/evidence graph"));
    assert!(
        !compaction_request_text.contains("Retained tail sentinel"),
        "retained raw tail must stay out of the compactor request"
    );

    let snapshot = ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("context compiles")
        .to_snapshot();

    for expected in [
        "Compaction is required",
        "Do not use an artifact/evidence graph",
        "Runtime cannot validate open-ended semantic truth",
        "Do not store every artifact and event forever",
        "Function-call continuity remains raw until compaction",
        "Retained raw tail is not part of compaction input",
        "Pins, modes, resource indexes, and artifact graphs are not the current core solution",
        "citation-backed checkpoint claims and local ref lookup",
        "working_intent",
    ] {
        assert!(
            snapshot.contains(expected),
            "missing expected checkpoint meaning: {expected}"
        );
    }

    let excerpt = runtime
        .read_checkpoint_ref(
            outcome.checkpoint_id(),
            &CheckpointRefId::new("r3").expect("valid ref id"),
        )
        .await
        .expect("ref resolves");
    assert!(
        excerpt
            .excerpt()
            .contains("Do not make the artifact/evidence graph")
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires live OpenAI-compatible compactor; set MERRY_OPENAI_LIVE_TESTS=1, MERRY_OPENAI_API_KEY or OPENAI_API_KEY, and MERRY_OPENAI_MODEL"]
async fn live_compactor_summarizes_messy_1k_token_window_with_refs() {
    let Some((compactor, compaction_model)) = live_openai_provider_from_env() else {
        return;
    };
    let messages = messy_live_compaction_user_messages();
    let source_text = messages.join("\n");
    let source_tokens = rough_token_count(&source_text);
    assert!(
        source_tokens >= 1_000,
        "live fixture must be large enough to judge compaction quality; got {source_tokens}"
    );

    let mut primary_steps = Vec::with_capacity(messages.len());
    for index in 0..messages.len() {
        primary_steps.push(vec![Ok(completed_text_event(&format!(
            "acknowledged messy input {index}"
        )))]);
    }
    let primary = ScriptedModelProvider::new(primary_steps);
    let runtime = Runtime::builder(session_id("live-compactor-quality"))
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            compaction_model.clone(),
        )
        .build()
        .expect("runtime builds");

    for message in &messages {
        let events = collect_step(&runtime, message).await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
            "seed message should complete"
        );
    }

    let policy =
        CitationCompactionPolicy::new(420, None, 12_000, 4, 1200, 16).expect("valid policy");
    let input = runtime
        .citation_compaction_input(policy)
        .await
        .expect("compaction input builds")
        .expect("compaction input exists");
    let payload = input
        .to_model_payload_json()
        .expect("compaction payload serializes");
    let response_format = ModelResponseFormat::StructuredOutput(
        ModelStructuredOutputFormat::new(
            "compacted_checkpoint_candidate",
            citation_compaction_response_schema(),
        )
        .expect("structured output format is valid"),
    );
    let request = ModelRequest::new_with_continuations_and_stable_prefix_and_response_format(
        compaction_model,
        vec![
            ModelMessage::new(
                ModelMessageRole::System,
                ModelContent::text(citation_compaction_system_prompt())
                    .expect("system prompt is valid"),
            )
            .expect("system message is valid"),
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text(&payload).expect("payload is valid model content"),
            )
            .expect("user message is valid"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::new(policy.model_output_token_limit(), false)
            .expect("generation config is valid"),
        1,
        Some(response_format),
    )
    .expect("compaction request is valid");
    let stream = compactor
        .stream_model(request, ModelStreamContext::new(CancellationToken::new()))
        .await
        .expect("live compactor stream starts");
    let candidate_json = collect_text_output(stream)
        .await
        .expect("live compactor returns text");
    eprintln!("live compactor raw candidate:\n{candidate_json}");
    let outcome = runtime
        .install_citation_compaction_candidate(input, &candidate_json)
        .await
        .expect("live compaction candidate installs");

    let snapshot = ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("context compiles")
        .to_snapshot();
    let checkpoint = snapshot.as_str();
    let checkpoint_tokens = rough_token_count(checkpoint);

    eprintln!("live compactor source_tokens={source_tokens}");
    eprintln!(
        "live compactor covered_history_items={}",
        outcome.covered_history_item_count()
    );
    eprintln!("live compactor checkpoint_tokens={checkpoint_tokens}");
    eprintln!("live compactor checkpoint:\n{checkpoint}");

    assert!(
        checkpoint_tokens * 3 < source_tokens,
        "live checkpoint should approach 3x compression after prompt-budget tuning"
    );
    let claim_count = checkpoint.matches("\n- c").count();
    assert!(
        claim_count <= 8,
        "live checkpoint should prefer 6-8 claims, got {claim_count}"
    );
    let checkpoint_lowercase = checkpoint.to_lowercase();
    for (expected, alternatives) in [
        ("artifact", &["artifact"][..]),
        ("runtime", &["runtime"][..]),
        (
            "tool/function-call continuity",
            &[
                "function-call",
                "function call",
                "tool call",
                "tool result",
                "continuity",
            ][..],
        ),
        ("retained raw tail", &["retained raw tail", "raw tail"][..]),
        ("citation", &["citation", "ref", "refs"][..]),
    ] {
        assert!(
            contains_any(&checkpoint_lowercase, alternatives),
            "live checkpoint missed expected design signal: {expected}"
        );
    }
    if let Some(working_intent) = checkpoint
        .lines()
        .find(|line| line.starts_with("working_intent "))
    {
        let working_intent = working_intent.to_lowercase();
        for forbidden in [
            "produce this checkpoint",
            "checkpoint candidate",
            "summarize the covered window",
            "compactor",
        ] {
            assert!(
                !working_intent.contains(forbidden),
                "working_intent must describe main-agent continuation, not compactor work"
            );
        }
    }
    assert!(
        !checkpoint.contains("Retained tail sentinel"),
        "live checkpoint must not summarize retained raw tail"
    );

    let excerpt = runtime
        .read_checkpoint_ref(
            outcome.checkpoint_id(),
            &CheckpointRefId::new("r3").expect("valid ref id"),
        )
        .await
        .expect("ref resolves");
    eprintln!("live compactor sample r3 excerpt:\n{}", excerpt.excerpt());
}

#[tokio::test(flavor = "current_thread")]
async fn installed_checkpoint_replaces_old_body_but_keeps_raw_tail_in_next_request() {
    let provider = FakeModelProvider::new(vec![
        Ok(completed_text_event("old assistant")),
        Ok(completed_event()),
        Ok(completed_event()),
    ]);
    let runtime = Runtime::builder(session_id("checkpoint-install-request"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "old user").await;
    collect_step(&runtime, "tail user").await;

    let input = runtime
        .citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 2, 1200, 16).expect("valid policy"),
        )
        .await
        .expect("input builds")
        .expect("input exists");

    runtime
        .install_citation_compaction_candidate(
            input,
            r#"{
              "claims": [
                {
                  "id": "c1",
                  "kind": "completed_action",
                  "text": "The old request was compacted.",
                  "refs": ["r1", "r2"]
                }
              ],
              "working_intent": null
            }"#,
        )
        .await
        .expect("install succeeds");

    collect_step(&runtime, "current user").await;
    let requests = provider.recorded_requests();
    let request = requests.last().expect("request exists");
    let text = request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("compacted-checkpoint:"));
    assert!(text.contains("The old request was compacted."));
    assert!(text.contains("tail user"));
    assert!(text.contains("current user"));
    assert!(!text.contains("\nold user\n"));
}

#[tokio::test(flavor = "current_thread")]
async fn dynamic_context_projection_keeps_checkpoint_tail_and_current_input_outside_stable_prefix()
{
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("covered assistant sentinel"))],
        vec![Ok(completed_text_event("tail assistant one sentinel"))],
        vec![Ok(completed_text_event("tail assistant two sentinel"))],
        vec![Ok(completed_event())],
    ]);
    let runtime = Runtime::builder(session_id("checkpoint-dynamic-projection"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Stable project rules sentinel.")
                .expect("valid project rules"),
        )
        .task_anchor(
            TaskAnchor::new("Keep implementing dynamic context projection.")
                .expect("valid task anchor"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(
        &runtime,
        "covered user sentinel should only remain reachable through checkpoint refs",
    )
    .await;
    collect_step(&runtime, "tail user one sentinel").await;
    collect_step(&runtime, "tail user two sentinel").await;

    let input = runtime
        .citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 4, 1200, 16).expect("valid policy"),
        )
        .await
        .expect("input builds")
        .expect("input exists");

    let outcome = runtime
        .install_citation_compaction_candidate(
            input,
            r#"{
              "claims": [
                {
                  "id": "c1",
                  "kind": "completed_action",
                  "text": "The covered request was compacted.",
                  "refs": ["r1", "r2"]
                }
              ],
              "working_intent": null
            }"#,
        )
        .await
        .expect("install succeeds");

    collect_step(&runtime, "current user sentinel").await;
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 4);

    let before_compaction = &requests[2];
    let after_compaction = requests.last().expect("request exists");
    assert_eq!(before_compaction.stable_prefix_message_count(), 2);
    assert_eq!(after_compaction.stable_prefix_message_count(), 2);
    assert_eq!(
        before_compaction.stable_prefix_hash(),
        after_compaction.stable_prefix_hash(),
        "checkpoint, raw tail, and current input are dynamic context, not stable prefix"
    );
    assert_ne!(
        before_compaction.dynamic_context_hash(),
        after_compaction.dynamic_context_hash(),
        "installing a checkpoint and adding current input should change dynamic context"
    );

    let stable_text = after_compaction
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(stable_text.contains("Stable project rules sentinel."));
    assert!(!stable_text.contains("compacted-checkpoint:"));
    assert!(!stable_text.contains("tail user one sentinel"));
    assert!(!stable_text.contains("current user sentinel"));

    let dynamic = after_compaction.dynamic_messages();
    assert_eq!(
        dynamic
            .iter()
            .map(|message| message.role())
            .collect::<Vec<_>>(),
        [
            ModelMessageRole::System,
            ModelMessageRole::System,
            ModelMessageRole::User,
            ModelMessageRole::Assistant,
            ModelMessageRole::User,
            ModelMessageRole::Assistant,
            ModelMessageRole::User,
        ]
    );

    let dynamic_text = dynamic
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>();
    assert!(dynamic_text[0].contains("task-anchor:"));
    assert!(dynamic_text[1].contains("compacted-checkpoint:"));
    assert!(dynamic_text[1].contains("The covered request was compacted."));
    assert!(dynamic_text[1].contains("[r1,r2]"));
    assert_eq!(dynamic_text[2], "tail user one sentinel");
    assert_eq!(dynamic_text[3], "tail assistant one sentinel");
    assert_eq!(dynamic_text[4], "tail user two sentinel");
    assert_eq!(dynamic_text[5], "tail assistant two sentinel");
    assert_eq!(dynamic_text[6], "current user sentinel");

    let request_text = dynamic_text.join("\n");
    assert!(!request_text.contains("covered user sentinel"));
    assert!(!request_text.contains("covered assistant sentinel"));
    assert!(after_compaction.continuations().is_empty());

    let ref_excerpt = runtime
        .read_checkpoint_ref(
            outcome.checkpoint_id(),
            &CheckpointRefId::new("r1").expect("valid ref id"),
        )
        .await
        .expect("checkpoint ref resolves");
    assert!(
        ref_excerpt
            .excerpt()
            .contains("covered user sentinel should only remain reachable through checkpoint refs")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_model_request_excludes_retained_tail_and_tools() {
    let compactor = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("old compacted assistant sentinel"))],
        vec![Ok(completed_text_event("tail assistant sentinel"))],
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::text(
                r#"{
                  "claims": [
                    {
                      "id": "c1",
                      "kind": "completed_action",
                      "text": "Old history was compacted.",
                      "refs": ["r1", "r2"]
                    }
                  ],
                  "working_intent": null
                }"#,
            )],
            FinishReason::Stop,
        ))],
    ]);
    let runtime = Runtime::builder(session_id("compaction-request-tail"))
        .model_provider(Arc::new(compactor.clone()), model_name())
        .build()
        .expect("runtime builds");

    seed_history_text_for_compaction(
        &runtime,
        "old compacted user sentinel",
        "retained raw tail sentinel",
    )
    .await;

    runtime
        .compact_context_once(
            CitationCompactionPolicy::new(128, None, 4096, 2, 1200, 16).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("compaction succeeds");

    let requests = compactor.recorded_requests();
    let request_text = requests[2]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(request_text.contains("old compacted user sentinel"));
    assert!(request_text.contains("old compacted assistant sentinel"));
    assert!(!request_text.contains("retained raw tail sentinel"));
    assert!(requests[2].tools().is_empty());
    assert!(requests[2].continuations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn compacted_checkpoint_renders_after_task_anchor_before_transcript_body() {
    let provider = FakeModelProvider::new(vec![
        Ok(completed_text_event("transcript assistant sentinel")),
        Ok(completed_event()),
    ]);
    let runtime = Runtime::builder(session_id("provider-compacted-checkpoint-order"))
        .task_anchor(TaskAnchor::new("task anchor sentinel").expect("valid task anchor"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("compacted checkpoint sentinel")
                .expect("valid compacted checkpoint"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "transcript user sentinel").await;
    collect_step(&runtime, "current user sentinel").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>();

    let task_anchor_index = messages
        .iter()
        .position(|text| text.contains("task anchor sentinel"))
        .expect("task anchor should render");
    let checkpoint_index = messages
        .iter()
        .position(|text| text.contains("compacted checkpoint sentinel"))
        .expect("compacted checkpoint should render");
    let append_user_index = messages
        .iter()
        .position(|text| text.contains("transcript user sentinel"))
        .expect("transcript user body should render");
    let append_assistant_index = messages
        .iter()
        .position(|text| text.contains("transcript assistant sentinel"))
        .expect("transcript assistant body should render");
    let current_user_index = messages
        .iter()
        .position(|text| text.contains("current user sentinel"))
        .expect("current user input should render");

    assert_eq!(requests[1].stable_prefix_message_count(), 1);
    assert!(task_anchor_index < checkpoint_index);
    assert!(checkpoint_index < append_user_index);
    assert!(append_user_index < append_assistant_index);
    assert!(append_assistant_index < current_user_index);
    assert!(
        messages[checkpoint_index].contains("compacted-checkpoint:"),
        "checkpoint should be marked as compacted checkpoint context"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compacted_checkpoint_does_not_project_unrelated_artifact_payloads() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = Runtime::builder(session_id("provider-compacted-checkpoint-boundary"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("compacted checkpoint payload")
                .expect("valid compacted checkpoint"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    let payload = "unrelated artifact payload sentinel must stay out";
    runtime
        .record_artifact(
            ArtifactRef::new(artifact_id("unrelated-artifact"), ArtifactKind::Text),
            ArtifactContent::text(payload),
        )
        .await
        .expect("artifact should record");

    collect_step(&runtime, "Answer with compacted checkpoint only.").await;

    let request = provider.recorded_requests()[0].clone();
    assert!(request.messages().iter().any(|message| {
        message
            .content()
            .as_text()
            .contains("compacted checkpoint payload")
    }));
    assert!(
        request
            .messages()
            .iter()
            .all(|message| !message.content().as_text().contains(payload)),
        "compacted checkpoint must not sweep unrelated artifact payloads into prompt"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ledger_observations_do_not_enter_prompt_context_by_default() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_event())],
    ]);
    let runtime = Runtime::builder(session_id("provider-ledger-not-projected"))
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("search_notes"),
            Arc::new(ScriptedToolExecutor::succeeding_text(
                "ledger projection sentinel must stay out of prompt\n",
            )),
        ))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Request tool result.").await;
    let pending = pending_tool_call(&pending_events).clone();
    runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");
    collect_step(&runtime, "Use the resolved tool result.").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].stable_prefix_message_count(), 1);
    assert!(
        requests[1].messages().iter().all(|message| {
            let text = message.content().as_text();
            !text.contains("ledger projection sentinel")
                && !text.contains("tool_result_observation")
                && !text.contains("Ledger")
        }),
        "ledger/tool-result observations must not be rendered into prompt messages by default"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn artifact_payloads_do_not_enter_prompt_context_by_default() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-artifact-not-projected", provider.clone());
    let payload = "artifact payload sentinel must stay out of prompt";
    runtime
        .record_artifact(
            ArtifactRef::new(artifact_id("artifact-not-projected"), ArtifactKind::Text),
            ArtifactContent::text(payload),
        )
        .await
        .expect("artifact should record");

    collect_step(&runtime, "Answer without compacted checkpoint context.").await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].stable_prefix_message_count(), 1);
    assert!(
        requests[0]
            .messages()
            .iter()
            .all(|message| !message.content().as_text().contains(payload)),
        "recorded artifact payload must not be rendered into prompt messages by default"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_process_runner_executes_read_only_shell_wrapper_with_input_artifact() {
    if !command_is_available("bash") || !command_is_available("wc") {
        return;
    }

    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(shell_process_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = Runtime::builder(session_id("provider-real-shell-runner"))
        .model_provider(Arc::new(provider), model_name())
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a local process from argv through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .allow_read_only_shell_process_actions(Arc::new(TokioProcessRunner::new()))
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Run read-only shell pipeline.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("read-only shell wrapper should execute through the Tokio runner");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
    );
    let input_artifact = execution_events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ArtifactRecorded { artifact }
                if artifact.id().as_str().starts_with("process-input-") =>
            {
                Some(artifact)
            }
            _ => None,
        })
        .expect("shell input artifact should be recorded before result");
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);

    let input_content = runtime
        .read_artifact_content(input_artifact.id())
        .await
        .expect("input artifact should be readable");
    let input_text = input_content
        .as_text()
        .expect("input artifact should be textual JSON");
    let input_payload: Value = serde_json::from_str(input_text).expect("input JSON should parse");
    assert_eq!(input_payload["kind"], "shell_command_input");
    assert_eq!(
        input_payload["permission_profile_id"],
        "process.shell.read_only.v1"
    );
    assert_eq!(
        input_payload["input_evidence"]["script"],
        "echo ProcessRunner | wc -l"
    );

    let result_content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("result artifact should be readable");
    let result_text = result_content
        .as_text()
        .expect("result artifact should be textual JSON");
    let result_payload: Value =
        serde_json::from_str(result_text).expect("result JSON should parse");
    assert_eq!(
        result_payload["permission_profile_id"],
        "process.shell.read_only.v1"
    );
    assert_eq!(
        result_payload["input_artifact"],
        json!({
            "id": input_artifact.id().as_str(),
            "kind": "json",
        })
    );
    assert!(result_payload.get("input_evidence").is_none());
    assert_eq!(result_payload["stdout"]["text"], "1\n");
    assert_eq!(result_payload["stderr"]["text"], "");
}

#[tokio::test(flavor = "current_thread")]
async fn execute_registered_tool_success_records_artifact_resolves_and_compiles_continuation() {
    let call = model_tool_call_with_args(
        "call-success",
        "search_notes",
        Map::from_iter([("query".to_owned(), Value::String("alpha".to_owned()))]),
    );
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call.clone())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used tool result"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-success",
        provider.clone(),
        executor.clone(),
    );

    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending = pending_tool_call(&pending_events).clone();
    let reserved_artifact = ArtifactRef::new(artifact_id("tool-result-4"), ArtifactKind::Text);
    let before_reserved = runtime.ledger_projection().await;
    let reserved_err = runtime
        .record_artifact(
            reserved_artifact.clone(),
            ArtifactContent::text("external shadow result\n"),
        )
        .await
        .expect_err("external recording should not use runtime-owned tool result ids");
    let after_reserved = runtime.ledger_projection().await;

    assert!(matches!(
        reserved_err,
        merry_runtime::RuntimeError::ReservedArtifactId { artifact_id }
            if artifact_id == *reserved_artifact.id()
    ));
    assert_eq!(before_reserved, after_reserved);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending.clone()]);
    let evidence_err = runtime
        .evidence_ref(reserved_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("reserved tool result id must not be externally recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == *reserved_artifact.id()
    ));

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &execution_events[1].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.artifact().kind(), &ArtifactKind::Text);
    assert_eq!(result.call_id(), pending.id());
    let evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("executor result artifact should be readable after ArtifactRecorded");
    assert_eq!(evidence.artifact_id, *result.artifact().id());
    assert_eq!(executor.calls(), vec![pending.clone()]);
    let projection = runtime.ledger_projection().await;
    assert_eq!(
        projection.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::ToolCallResolved,
            },
        ]
    );

    let continuation_events = collect_step(&runtime, "Continue with result.").await;
    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert_eq!(
        continuation_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6, 7]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_tools_are_default_checkpoint_ref_plus(
        requests[0].tools(),
        &[test_tool_spec("search_notes")],
    );
    assert!(requests[0].continuations().is_empty());
    assert_tools_are_default_checkpoint_ref_plus(
        requests[1].tools(),
        &[test_tool_spec("search_notes")],
    );
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("tool result continuation should be compiled");
    assert_eq!(continuation.call().id().as_str(), "call-success");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(
        continuation.result().content().as_text(),
        Some("search result\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reading_catalog_skill_file_emits_skill_used_event() {
    let call = model_tool_call_with_args(
        "call-read-skill",
        "workspace_read_file",
        Map::from_iter([("path".to_owned(), Value::String("demo/SKILL.md".to_owned()))]),
    );
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let catalog = SkillCatalog::from_metadata(vec![
        SkillMetadata::new(
            "demo-skill",
            "Use for demo tasks.",
            PathBuf::from("demo/SKILL.md"),
            PathBuf::from("/skills"),
        )
        .expect("valid skill metadata"),
    ])
    .expect("valid skill catalog");
    let runtime = Runtime::builder(session_id("provider-skill-used"))
        .skill_catalog(catalog)
        .register_tool(RegisteredTool::read_only(
            path_tool_spec("workspace_read_file"),
            Arc::new(ScriptedToolExecutor::succeeding_text("# Demo\n")),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let pending_events = collect_step(&runtime, "Use demo skill.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved", "SkillUsed"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[2].payload,
        RuntimeJournalPayload::SkillUsed {
            skill_name,
            skill_md_path,
            tool_call_id,
            artifact,
        } if skill_name == "demo-skill"
            && skill_md_path == "demo/SKILL.md"
            && tool_call_id == pending.id()
            && artifact == result.artifact()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_domain_failure_resolves_failed_without_runtime_failed() {
    let call = model_tool_call_with_id("call-domain-failure");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::failing_json("tool_lookup_failed", r#"{"ok":false}"#);
    let runtime = runtime_with_registered_tool("provider-execute-tool-failure", provider, executor);
    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("domain failure should resolve the pending call");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(
        execution_events
            .iter()
            .all(|event| !matches!(event.payload, RuntimeJournalPayload::Failed { .. })),
        "tool domain failure must not emit RuntimeJournalPayload::Failed: {execution_events:?}"
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &execution_events[1].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert_eq!(result.call_id(), pending.id());
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should have diagnostic")
            .code(),
        "tool_lookup_failed"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("domain failure artifact should be readable after ArtifactRecorded");
    assert_eq!(evidence.artifact_id, *result.artifact().id());
    let projection = runtime.ledger_projection().await;
    assert_eq!(
        projection.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::ToolCallResolved,
            },
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn executor_infrastructure_error_keeps_pending_without_artifact_or_result() {
    let call = model_tool_call_with_id("call-infra-failure");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::infrastructure_error("temporary executor outage");
    let runtime =
        runtime_with_registered_tool("provider-execute-tool-infra-error", provider, executor);
    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;
    assert_eq!(
        before.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
        ]
    );

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("infrastructure failure should not resolve the pending call");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::ToolExecutionFailed { call_id, .. }
            if call_id == *pending.id()
    ));
    assert_eq!(before, after);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    let evidence_err = runtime
        .evidence_ref(
            &artifact_id("tool-result-3"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("infrastructure failure must not record runtime-owned tool result artifact");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("tool-result-3")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_blank_text_outcome_keeps_pending_without_artifact_or_result() {
    let call = model_tool_call_with_id("call-blank-text-outcome");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::succeeding_text(" \n\t ");
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-blank-text-outcome",
        provider,
        executor,
    );
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("blank executor text should not resolve the pending call");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: ArtifactContentKind::Text
        } if artifact_id.as_str() == "tool-result-3"
    ));
    assert_eq!(before, after);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    let evidence_err = runtime
        .evidence_ref(
            &artifact_id("tool-result-3"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("blank executor outcome must not record an artifact");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("tool-result-3")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_blank_json_outcome_keeps_pending_without_artifact_or_result() {
    let call = model_tool_call_with_id("call-blank-json-outcome");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::new(ToolExecutorResponse::Outcome(
        ToolExecutionOutcome::succeeded_json(" \n\t "),
    ));
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-blank-json-outcome",
        provider,
        executor,
    );
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("blank executor JSON should not resolve the pending call");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: ArtifactContentKind::Json
        } if artifact_id.as_str() == "tool-result-3"
    ));
    assert_eq!(before, after);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    let evidence_err = runtime
        .evidence_ref(
            &artifact_id("tool-result-3"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("blank executor outcome must not record an artifact");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("tool-result-3")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn executor_reentrant_runtime_mutations_are_rejected_while_outer_execution_resolves() {
    let call = model_tool_call_with_id("call-reentrant-executor");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ReentrantMutationExecutor::new();
    let runtime = Runtime::builder(session_id("provider-execute-tool-reentrant"))
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("search_notes"),
            Arc::new(executor.clone()),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    executor.set_runtime(runtime.clone());
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("outer registered executor result should resolve");

    assert_eq!(
        executor.observations(),
        [
            ReentrantMutationObservation::RecordStepAlreadyActive,
            ReentrantMutationObservation::SubmitStepAlreadyActive,
        ]
    );
    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.call_id(), pending.id());
    assert!(runtime.pending_tool_calls().await.is_empty());
    let outer_evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("outer executor result artifact should be readable");
    assert_eq!(outer_evidence.artifact_id, *result.artifact().id());
    let inner_evidence_err = runtime
        .evidence_ref(
            &artifact_id("executor-inner-artifact"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("reentrant executor artifact must not be recorded");
    assert!(matches!(
        inner_evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("executor-inner-artifact")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn unregistered_pending_tool_name_resolves_failed_with_tool_not_registered() {
    let call = model_tool_call_with_args("call-unregistered", "missing_tool", Map::new());
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after missing tool"))],
    ]);
    let runtime = Runtime::builder(session_id("provider-execute-tool-unregistered"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    let pending_events = collect_step(&runtime, "Call missing tool.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("unregistered tool should synthesize failed result");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &execution_events[1].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should have diagnostic")
            .code(),
        "tool_not_registered"
    );
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert_eq!(result.call_id(), pending.id());
    assert!(failed_code(&execution_events).is_none());
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("unregistered tool failure artifact should be readable after ArtifactRecorded");
    assert_eq!(evidence.artifact_id, *result.artifact().id());
    let projection = runtime.ledger_projection().await;
    assert_eq!(
        projection.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::ToolCallResolved,
            },
        ]
    );

    let continuation_events = collect_step(&runtime, "Continue after missing tool.").await;
    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert_eq!(
        continuation_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6, 7]
    );
    assert_eq!(provider.recorded_requests()[1].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn denied_registered_tool_result_is_compiled_as_failed_provider_neutral_continuation() {
    let call = model_tool_call_with_id("call-policy-denied");
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after policy denial"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("executor must not run\n");
    let runtime = runtime_with_registered_tool_action(
        "provider-policy-denied-continuation",
        provider.clone(),
        executor.clone(),
        ToolActionKind::WorkspaceWrite,
    );
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("policy denial should resolve the pending call");
    let continuation_events = collect_step(&runtime, "Continue after denial.").await;

    assert_eq!(executor.calls().len(), 0);
    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("policy denial result should include diagnostic")
            .code(),
        "action_policy_denied"
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("failed policy denial should be compiled as continuation");
    assert_eq!(continuation.call().id().as_str(), "call-policy-denied");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .map(merry_core::ErrorInfo::code),
        Some("action_policy_denied")
    );
    let content = continuation
        .result()
        .content()
        .as_json()
        .expect("policy denial continuation should carry JSON content");
    let value: Value = serde_json::from_str(content).expect("denial JSON should parse");
    assert_sanitized_policy_denial_json(&value, "search_notes");

    let serialized = serde_json::to_value(continuation).expect("continuation should serialize");
    assert!(serialized.get("provider").is_none());
    assert!(serialized.get("wire").is_none());
    assert!(serialized.get("previous_response_id").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_with_bad_or_missing_args_resolves_schema_failure_before_executor() {
    let call = model_tool_call_with_args("call-bad-args", "search_notes", Map::new());
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::succeeding_text("accepted bad args\n");
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-schema-pass",
        provider,
        executor.clone(),
    );
    let pending_events = collect_step(&runtime, "Search with missing args.").await;
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("schema failure should resolve the pending call");

    let calls = executor.calls();
    assert_eq!(calls.len(), 0);
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("schema failure should carry diagnostic")
            .code(),
        "tool_input_schema_invalid"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_multiple_tool_call_requests_fail_without_partial_pending() {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-1"),
        }),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-2"),
        }),
        Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-1"))],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-streamed-multiple", provider);

    let events = collect_step(&runtime, "Request multiple streamed tools.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("model_parallel_tool_calls_unsupported")
    );
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_tool_call_then_stop_text_fails_without_artifact_or_pending() {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-1"),
        }),
        Ok(completed_outputs_event(
            vec![ModelOutput::text("fallback text")],
            FinishReason::Stop,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-then-stop-text", provider);

    let events = collect_step(&runtime, "Request tool then stop with text.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_tool_call_mixed_output"));
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_tool_call_then_completed_different_tool_call_fails_without_partial_pending()
 {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-1"),
        }),
        Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-2"))],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-completed-different", provider);

    let events = collect_step(&runtime, "Request one tool and complete another.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("model_parallel_tool_calls_unsupported")
    );
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_cancelled_stream_emits_cancelled_without_completion() {
    let provider = FakeModelProvider::new(vec![Err(merry_llm::ModelError::Cancelled)]);
    let runtime = runtime_with_provider("provider-cancelled", provider);

    let events = collect_step(&runtime, "Cancel in provider.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Cancelled"]
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::Cancelled { .. }))
    );
    assert_no_artifact_recorded(&events);
    assert_no_failed(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_single_tool_call_emits_pending_without_completion() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-finish-tool-calls", provider);

    let events = collect_step(&runtime, "Finish with tool calls.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(pending_tool_call(&events).id().as_str(), "call-1");
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
    assert!(failed_code(&events).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_tool_call_pending_preserves_id_name_arguments_and_ledger_fact() {
    let arguments = Map::from_iter([
        ("query".to_owned(), json!("runtime tool calls")),
        ("limit".to_owned(), json!(3)),
        ("include_archived".to_owned(), json!(false)),
    ]);
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call_with_args(
            "call.provider/opaque.id:42",
            "search_notes",
            arguments.clone(),
        ))],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-payload", provider);

    let events = collect_step(&runtime, "Search notes.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let call = pending_tool_call(&events);
    assert_eq!(call.id().as_str(), "call.provider/opaque.id:42");
    assert_eq!(call.name().as_str(), "search_notes");
    assert_eq!(call.arguments().as_object(), &arguments);
    let mut pending = runtime.pending_tool_calls().await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id().as_str(), "call.provider/opaque.id:42");
    assert_eq!(pending[0].name().as_str(), "search_notes");
    assert_eq!(pending[0].arguments().as_object(), &arguments);

    pending.clear();
    let pending_after_caller_mutation = runtime.pending_tool_calls().await;
    assert_eq!(pending_after_caller_mutation, vec![call.clone()]);

    let projection = runtime.ledger_projection().await;
    assert_eq!(
        projection.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unresolved_pending_tool_call_blocks_next_provider_step_without_calling_provider() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("must not be requested"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-pending-blocks-step", provider.clone());

    let first_events = collect_step(&runtime, "Request a tool.").await;
    let second_events = collect_step(&runtime, "Try to continue without tool result.").await;

    assert_eq!(
        event_kind_names(&first_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
    assert_eq!(
        failed_code(&second_events),
        Some("tool_call_result_required")
    );
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        runtime
            .pending_tool_calls()
            .await
            .iter()
            .map(|call| call.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["call-1".to_owned()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_provider_tool_call_id_after_pending_fails_without_second_pending() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-duplicate-id", provider.clone());

    let first_events = collect_step(&runtime, "Request a tool.").await;
    let second_events = collect_step(&runtime, "Request the same tool id again.").await;

    assert_eq!(
        event_kind_names(&first_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
    assert_eq!(
        failed_code(&second_events),
        Some("tool_call_result_required")
    );
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        runtime
            .pending_tool_calls()
            .await
            .iter()
            .map(|call| call.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["call-1".to_owned()]
    );

    let projection = runtime.ledger_projection().await;
    assert_eq!(
        projection.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::Failed,
            },
        ]
    );

    let result_artifact = ArtifactRef::new(
        artifact_id("manual-result-after-duplicate-pending"),
        ArtifactKind::Text,
    );
    let result = ToolCallResult::succeeded(
        ToolCallId::new("call-1").expect("valid call id"),
        result_artifact.clone(),
    );
    let resolved_events = runtime
        .submit_tool_result(
            result.clone(),
            ArtifactContent::text("only accepted once\n"),
        )
        .await
        .expect("single pending call should resolve");
    let duplicate_result = ToolCallResult::succeeded(
        ToolCallId::new("call-1").expect("valid call id"),
        ArtifactRef::new(
            artifact_id("manual-result-after-duplicate-second"),
            ArtifactKind::Text,
        ),
    );
    let duplicate_err = runtime
        .submit_tool_result(
            duplicate_result,
            ArtifactContent::text("must not resolve twice\n"),
        )
        .await
        .expect_err("resolved call id should reject duplicate result");

    assert_eq!(
        event_kind_names(&resolved_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert!(matches!(
        duplicate_err,
        merry_runtime::RuntimeError::ToolCallAlreadyResolved { call_id, .. }
            if call_id == ToolCallId::new("call-1").expect("valid call id")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_records_success_artifact_resolves_pending_and_updates_ledger() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after tool result"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-tool-result-success", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact =
        ArtifactRef::new(artifact_id("manual-result-success"), ArtifactKind::Text);
    let result = ToolCallResult::succeeded(call.id().clone(), result_artifact.clone());

    let events = runtime
        .submit_tool_result(result.clone(), ArtifactContent::text("exact tool output\n"))
        .await
        .expect("tool result should resolve");

    assert_eq!(
        event_kind_names(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(matches!(
        &events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == &result_artifact
    ));
    assert!(matches!(
        &events[1].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == &result
    ));
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence = runtime
        .evidence_ref(result_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("tool result artifact should be readable");
    assert_eq!(evidence.artifact_id, *result_artifact.id());

    let projection = runtime.ledger_projection().await;
    assert_eq!(
        projection.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::ToolCallResolved,
            },
        ]
    );

    let next_events = collect_step(&runtime, "after tool result").await;
    assert_eq!(
        event_kind_names(&next_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert_eq!(provider.recorded_requests().len(), 2);
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6, 7]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_rejects_reserved_artifact_ids_without_mutation() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-reserved-submit", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;

    for reserved_id in ["tool-result-4", "assistant-output-4", "process-input-4"] {
        let artifact = ArtifactRef::new(artifact_id(reserved_id), ArtifactKind::Text);
        let result = ToolCallResult::succeeded(call.id().clone(), artifact.clone());
        let err = runtime
            .submit_tool_result(result, ArtifactContent::text("external shadow result\n"))
            .await
            .expect_err("external submit should not use runtime-owned artifact ids");
        let after = runtime.ledger_projection().await;

        assert!(matches!(
            err,
            merry_runtime::RuntimeError::ReservedArtifactId { artifact_id }
                if artifact_id == *artifact.id()
        ));
        assert_eq!(before, after);
        assert_eq!(runtime.pending_tool_calls().await, vec![call.clone()]);
        let evidence_err = runtime
            .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
            .await
            .expect_err("reserved submitted result artifact must not be recorded");
        assert!(matches!(
            evidence_err,
            merry_runtime::RuntimeError::Artifact {
                source: merry_runtime::ArtifactError::MissingArtifact { id }
            } if id == *artifact.id()
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn submitted_tool_result_is_compiled_as_provider_neutral_continuation() {
    let arguments = Map::from_iter([
        ("query".to_owned(), json!("runtime continuation")),
        ("limit".to_owned(), json!(2)),
    ]);
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_args(
                "call.provider/opaque.id:42",
                "search_notes",
                arguments.clone(),
            ))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after search"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-tool-continuation", provider.clone());
    let pending_events = collect_step(&runtime, "Request a search.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact = ArtifactRef::new(
        artifact_id("manual-result-continuation-text"),
        ArtifactKind::Text,
    );
    let result = ToolCallResult::succeeded(call.id().clone(), result_artifact.clone());
    runtime
        .submit_tool_result(result, ArtifactContent::text("exact search result\n"))
        .await
        .expect("tool result should resolve");

    let events = collect_step(&runtime, "Use the tool result.").await;

    assert_eq!(
        event_kind_names(&events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("continuation should be compiled");
    assert_eq!(
        continuation.call().id().as_str(),
        "call.provider/opaque.id:42"
    );
    assert_eq!(continuation.call().name().as_str(), "search_notes");
    assert_eq!(continuation.call().arguments().as_object(), &arguments);
    assert_eq!(
        continuation.result().call_id().as_str(),
        "call.provider/opaque.id:42"
    );
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(
        continuation.result().content().as_text(),
        Some("exact search result\n")
    );
    assert!(continuation.result().diagnostic().is_none());

    let value = serde_json::to_value(&requests[1]).expect("request should serialize");
    assert!(value.get("session_id").is_none());
    assert!(value.get("ledger_id").is_none());
    assert!(value.get("artifact_id").is_none());
    assert!(value.get("previous_response_id").is_none());
    assert!(value.get("store").is_none());
    assert!(value.get("tool_call_id").is_none());
    assert!(
        value["continuations"][0]["call"]
            .get("tool_call_id")
            .is_none()
    );
    assert!(
        value["continuations"][0]["result"]
            .get("tool_call_id")
            .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn successful_provider_step_keeps_tool_exchange_until_compaction() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used tool result"))],
        vec![Ok(completed_text_event("fresh request"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-exchange-success", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact = ArtifactRef::new(
        artifact_id("manual-result-tool-exchange-success"),
        ArtifactKind::Text,
    );
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(call.id().clone(), result_artifact),
            ArtifactContent::text("result remains raw before compaction\n"),
        )
        .await
        .expect("tool result should resolve");

    let continuation_events = collect_step(&runtime, "Use tool result.").await;
    let next_events = collect_step(&runtime, "Continue without compaction.").await;

    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert_eq!(
        event_kind_names(&next_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[2].continuations().len(),
        1,
        "successful provider completion is not checkpoint/compaction"
    );
    assert_eq!(
        requests[2].continuations()[0].call().id().as_str(),
        requests[1].continuations()[0].call().id().as_str()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn new_pending_tool_call_keeps_prior_tool_exchange() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-old"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-new"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used both results"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-new-pending", provider.clone());
    let pending_events = collect_step(&runtime, "Request first tool.").await;
    let old_call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                old_call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-before-new-pending"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("old result\n"),
        )
        .await
        .expect("old tool result should resolve");

    let new_pending_events = collect_step(&runtime, "Use old result and request another.").await;
    let new_call = pending_tool_call(&new_pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                new_call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-after-new-pending"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("new result\n"),
        )
        .await
        .expect("new tool result should resolve");
    let completed_events = collect_step(&runtime, "Use all resolved tool results.").await;

    assert_eq!(
        event_kind_names(&new_pending_events),
        ["StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_tool_call(&new_pending_events).id().as_str(),
        "call-new"
    );
    assert_eq!(
        event_kind_names(&completed_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-old"
    );
    assert_eq!(
        requests[2]
            .continuations()
            .iter()
            .map(|continuation| continuation.call().id().as_str())
            .collect::<Vec<_>>(),
        ["call-old", "call-new"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_new_tool_call_id_keeps_tool_exchange_for_retry() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-old"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-old"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("retry uses old result"))],
    ]);
    let runtime = runtime_with_scripted_provider(
        "provider-tool-continuation-duplicate-new-id",
        provider.clone(),
    );
    let pending_events = collect_step(&runtime, "Request first tool.").await;
    let old_call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                old_call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-before-duplicate-new-id"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("old result\n"),
        )
        .await
        .expect("old tool result should resolve");

    let duplicate_events = collect_step(&runtime, "Provider repeats resolved id.").await;
    let retry_events = collect_step(&runtime, "Retry after duplicate id.").await;

    assert_eq!(
        event_kind_names(&duplicate_events),
        ["StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&duplicate_events), Some("tool_call_duplicate"));
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-old"
    );
    assert_eq!(
        requests[2].continuations()[0].call().id().as_str(),
        "call-old"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_error_keeps_tool_exchange_for_retry() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Err(ModelError::provider(
            ProviderErrorKind::Protocol,
            "transient continuation failure",
        ))],
        vec![Ok(completed_text_event("retry succeeded"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-error", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-retry-after-error"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("retry me\n"),
        )
        .await
        .expect("tool result should resolve");

    let error_events = collect_step(&runtime, "Use result, provider fails.").await;
    let retry_events = collect_step(&runtime, "Retry with same result.").await;

    assert_eq!(event_kind_names(&error_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&error_events), Some("model_protocol"));
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_setup_error_keeps_tool_exchange_for_retry() {
    let provider = ScriptedModelProvider::new_steps(vec![
        ScriptedProviderStep::Stream(vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))]),
        ScriptedProviderStep::SetupError(ModelError::provider(
            ProviderErrorKind::Unavailable,
            "setup unavailable",
        )),
        ScriptedProviderStep::Stream(vec![Ok(completed_text_event("retry after setup"))]),
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-setup-error", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-retry-after-setup-error"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("retry after setup\n"),
        )
        .await
        .expect("tool result should resolve");

    let error_events = collect_step(&runtime, "Setup fails.").await;
    let retry_events = collect_step(&runtime, "Retry setup.").await;

    assert_eq!(event_kind_names(&error_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&error_events), Some("model_unavailable"));
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_cancel_keeps_tool_exchange_for_retry() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Err(ModelError::Cancelled)],
        vec![Ok(completed_text_event("retry after cancel"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-cancel", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-retry-after-cancel"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("retry after cancel\n"),
        )
        .await
        .expect("tool result should resolve");

    let cancel_events = collect_step(&runtime, "Provider cancels.").await;
    let retry_events = collect_step(&runtime, "Retry after cancel.").await;

    assert_eq!(
        event_kind_names(&cancel_events),
        ["StepStarted", "Cancelled"]
    );
    assert_no_failed(&cancel_events);
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_tool_result_after_resolved_does_not_mutate_session() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-duplicate", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let first_artifact = ArtifactRef::new(artifact_id("manual-result-first"), ArtifactKind::Text);
    let first = ToolCallResult::succeeded(call.id().clone(), first_artifact.clone());
    runtime
        .submit_tool_result(first, ArtifactContent::text("first result\n"))
        .await
        .expect("first result should resolve");
    let projection_before_duplicate = runtime.ledger_projection().await;
    let duplicate_artifact =
        ArtifactRef::new(artifact_id("manual-result-duplicate"), ArtifactKind::Text);
    let duplicate = ToolCallResult::succeeded(call.id().clone(), duplicate_artifact.clone());

    let err = runtime
        .submit_tool_result(duplicate, ArtifactContent::text("duplicate result\n"))
        .await
        .expect_err("duplicate result should be rejected");
    let projection_after_duplicate = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::ToolCallAlreadyResolved {
            session_id: rejected_session,
            call_id
        } if rejected_session == session_id("provider-tool-result-duplicate")
            && call_id == ToolCallId::new("call-1").expect("valid call id")
    ));
    assert_eq!(projection_before_duplicate, projection_after_duplicate);
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence_err = runtime
        .evidence_ref(duplicate_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("duplicate artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *duplicate_artifact.id()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn artifact_error_while_submitting_tool_result_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-artifact-error", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let duplicate_artifact =
        ArtifactRef::new(artifact_id("manual-result-conflict"), ArtifactKind::Text);
    runtime
        .record_artifact(
            duplicate_artifact.clone(),
            ArtifactContent::text("existing artifact\n"),
        )
        .await
        .expect("conflicting artifact should record before submit");
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(call.id().clone(), duplicate_artifact.clone());

    let err = runtime
        .submit_tool_result(result, ArtifactContent::text("replacement\n"))
        .await
        .expect_err("duplicate artifact id should reject submit");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::DuplicateId { id }
        } if id == *duplicate_artifact.id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);

    let next_events = collect_step(&runtime, "after duplicate artifact submit").await;
    assert_eq!(event_kind_names(&next_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&next_events), Some("tool_call_result_required"));
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn incompatible_tool_result_content_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-incompatible", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(
            artifact_id("manual-result-json-mismatch"),
            ArtifactKind::Json,
        ),
    );

    let err = runtime
        .submit_tool_result(
            result.clone(),
            ArtifactContent::text("not json content kind\n"),
        )
        .await
        .expect_err("content kind mismatch should reject submit");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::IncompatibleContent {
                id,
                artifact_kind,
                content_kind
            }
        } if id == *result.artifact().id()
            && artifact_kind == ArtifactKind::Json
            && content_kind == merry_runtime::ArtifactContentKind::Text
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("incompatible result artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *result.artifact().id()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn blank_text_tool_result_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-blank-text", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(artifact_id("manual-result-blank-text"), ArtifactKind::Text),
    );

    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::text(" \n\t "))
        .await
        .expect_err("blank text tool result should be rejected before resolution");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: merry_runtime::ArtifactContentKind::Text
        } if artifact_id == *result.artifact().id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("blank result artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *result.artifact().id()
    ));

    let next_events = collect_step(&runtime, "after blank text submit").await;
    assert_eq!(event_kind_names(&next_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&next_events), Some("tool_call_result_required"));
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn blank_json_tool_result_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-blank-json", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(artifact_id("manual-result-blank-json"), ArtifactKind::Json),
    );

    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::json(" \n\t "))
        .await
        .expect_err("blank json tool result should be rejected before resolution");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: merry_runtime::ArtifactContentKind::Json
        } if artifact_id == *result.artifact().id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("blank result artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *result.artifact().id()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_rejects_unsupported_content_kind_without_mutation() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-unsupported", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(artifact_id("manual-result-binary"), ArtifactKind::Binary),
    );

    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::binary([1, 2, 3]))
        .await
        .expect_err("binary tool result content is not accepted in MVP");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: merry_runtime::ArtifactContentKind::Binary
        } if artifact_id == *result.artifact().id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_empty_tool_call_args_succeeds() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call_with_args(
            "call-1",
            "search_notes",
            Map::new(),
        ))],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-empty-args", provider);

    let events = collect_step(&runtime, "Call with empty args.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert!(
        pending_tool_call(&events)
            .arguments()
            .as_object()
            .is_empty()
    );
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_multiple_tool_calls_fails_without_partial_pending() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![
            ModelOutput::tool_call(model_tool_call_with_id("call-1")),
            ModelOutput::tool_call(model_tool_call_with_id("call-2")),
        ],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-multiple", provider);

    let events = collect_step(&runtime, "Return multiple tool calls.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("model_parallel_tool_calls_unsupported")
    );
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_tool_calls_finish_but_no_tool_call_fails() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        Vec::new(),
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-missing", provider);

    let events = collect_step(&runtime, "Finish without tool call payload.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_tool_call_missing"));
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_profile_progress_commentary_adds_stable_prefix_guidance() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let profile = merry_runtime::RuntimeProfile::builder()
        .progress_commentary(true)
        .build()
        .expect("profile should build");
    let runtime = Runtime::builder(session_id("provider-progress-commentary-profile"))
        .with_profile(profile)
        .expect("profile should install")
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "Inspect progress commentary config.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.messages().len(), 3);
    assert_eq!(request.stable_prefix_message_count(), 2);
    assert_eq!(request.messages()[1].role(), ModelMessageRole::System);
    assert!(
        request.messages()[1]
            .content()
            .as_text()
            .contains("brief progress note first")
    );
    assert!(
        request.messages()[1]
            .content()
            .as_text()
            .contains("user's current input language")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_mixed_text_and_tool_call_records_commentary_and_pending() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![
            ModelOutput::text("partial answer"),
            ModelOutput::tool_call(model_tool_call()),
        ],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-mixed-output", provider);

    let events = collect_step(&runtime, "Mix text and tool call.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "ToolCallPending"
        ]
    );
    let artifact = assistant_output_artifact(&events);
    let content = runtime
        .read_artifact_content(artifact.id())
        .await
        .expect("commentary artifact should be readable");
    assert_eq!(content.as_text(), Some("partial answer"));
    assert_eq!(pending_tool_call(&events).id().as_str(), "call-1");
    assert_no_failed(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_tool_call_after_non_empty_text_delta_records_commentary_and_pending() {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::OutputTextDelta {
            delta: "thinking aloud".to_owned(),
        }),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call(),
        }),
        Ok(completed_outputs_event(
            vec![
                ModelOutput::text("thinking aloud"),
                ModelOutput::tool_call(model_tool_call()),
            ],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-after-text-delta", provider);

    let events = collect_step(&runtime, "Emit text before tool call.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "ToolCallPending"
        ]
    );
    let artifact = assistant_output_artifact(&events);
    let content = runtime
        .read_artifact_content(artifact.id())
        .await
        .expect("commentary artifact should be readable");
    assert_eq!(content.as_text(), Some("thinking aloud"));
    assert_eq!(pending_tool_call(&events).id().as_str(), "call-1");
    assert_no_failed(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn tool_progress_commentary_is_replayed_to_next_provider_step() {
    let provider = ScriptedModelProvider::new(vec![
        vec![
            Ok(ModelEvent::OutputTextDelta {
                delta: "I will inspect the notes first.".to_owned(),
            }),
            Ok(ModelEvent::ToolCallRequested {
                call: model_tool_call(),
            }),
            Ok(completed_outputs_event(
                vec![
                    ModelOutput::text("I will inspect the notes first."),
                    ModelOutput::tool_call(model_tool_call()),
                ],
                FinishReason::ToolCalls,
            )),
        ],
        vec![Ok(completed_text_event("continued"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-commentary-replay", provider.clone());

    let pending_events = collect_step(&runtime, "Need notes.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact =
        ArtifactRef::new(artifact_id("manual-result-commentary"), ArtifactKind::Text);
    let result = ToolCallResult::succeeded(call.id().clone(), result_artifact.clone());
    runtime
        .submit_tool_result(result, ArtifactContent::text("note result\n"))
        .await
        .expect("tool result should resolve");
    let final_events = collect_step(&runtime, "Continue.").await;

    assert_eq!(
        event_kind_names(&final_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].messages().iter().any(|message| {
            message.role() == ModelMessageRole::Assistant
                && message.content().as_text() == "I will inspect the notes first."
        }),
        "tool-progress commentary should be stored as assistant history for provider continuity"
    );
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].result().content().as_str(),
        "note result\n"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_cancelled_finish_emits_cancelled_without_step_completed() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event_with_finish(
        FinishReason::Cancelled,
    ))]);
    let runtime = runtime_with_provider("provider-finish-cancelled", provider);

    let events = collect_step(&runtime, "Finish cancelled.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Cancelled"]
    );
    assert_no_artifact_recorded(&events);
    assert_no_failed(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_error_finish_emits_failed_without_step_completed() {
    let provider =
        FakeModelProvider::new(vec![Ok(completed_event_with_finish(FinishReason::Error))]);
    let runtime = runtime_with_provider("provider-finish-error", provider);

    let events = collect_step(&runtime, "Finish with error.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_finish_error"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_length_finish_emits_failed_without_step_completed() {
    let provider =
        FakeModelProvider::new(vec![Ok(completed_event_with_finish(FinishReason::Length))]);
    let runtime = runtime_with_provider("provider-finish-length", provider);

    let events = collect_step(&runtime, "Finish with length.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_length"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_cancelled_kind_error_emits_cancelled_without_failed_or_completion() {
    let provider = FakeModelProvider::new(vec![Err(ModelError::provider(
        ProviderErrorKind::Cancelled,
        "provider cancelled request",
    ))]);
    let runtime = runtime_with_provider("provider-cancelled-kind", provider);

    let events = collect_step(&runtime, "Provider cancellation kind.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Cancelled"]
    );
    assert_no_artifact_recorded(&events);
    assert_no_failed(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_with_multiple_outputs_emits_model_output_unsupported_failed() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text("first"), ModelOutput::text("second")],
        FinishReason::Stop,
    ))]);
    let runtime = runtime_with_provider("provider-stop-multiple-outputs", provider);

    let events = collect_step(&runtime, "Return too many outputs.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_output_unsupported"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_with_tool_output_emits_model_output_unsupported_failed() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::Stop,
    ))]);
    let runtime = runtime_with_provider("provider-stop-tool-output", provider);

    let events = collect_step(&runtime, "Return a tool output with stop.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_output_unsupported"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_absent_step_does_not_compile_context_and_preserves_skeleton_behavior() {
    let runtime = Runtime::builder(session_id("provider-absent"))
        .build()
        .expect("runtime should build");
    runtime
        .record_context_summary(
            ContextSummary::new(
                "invalid-summary-without-provider",
                "This would fail if the provider path compiled context.",
                Vec::new(),
            )
            .expect("summary construction allows compiler validation"),
        )
        .await
        .expect("raw context summary should record");

    let events = collect_step(&runtime, "Run without provider.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "StepCompleted"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_no_artifact_recorded(&events);
}
