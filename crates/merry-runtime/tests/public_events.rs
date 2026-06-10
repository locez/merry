use futures_util::StreamExt;
use merry_core::{
    ArtifactKind, RuntimeEvent, RuntimeJournalEvent, RuntimeJournalPayload, SessionId,
    ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments, testing::FakeModelProvider,
};
use merry_runtime::{RegisteredTool, Runtime, StepContext, StepInput};
use schemars::Schema;
use serde_json::{Map, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

fn model_name() -> merry_llm::ModelName {
    merry_llm::ModelName::new("fake/model").expect("valid model name")
}

fn completed_text_event(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

fn completed_tool_call_event(call: ModelToolCall) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    }
}

fn model_tool_call(id: &str, name: &str) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid model tool call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::new(Map::from_iter([("query".to_owned(), json!("notes"))])),
    )
}

fn tool_spec(name: &str) -> ToolSpec {
    let schema =
        Schema::try_from(json!({ "type": "object" })).expect("test schema should be JSON schema");
    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Test tool",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

async fn collect_public_stream(runtime: &Runtime, text: &str) -> Vec<RuntimeEvent> {
    runtime
        .stream(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("public stream should start")
        .collect()
        .await
}

async fn collect_journal_stream(runtime: &Runtime, text: &str) -> Vec<RuntimeJournalEvent> {
    runtime
        .journal_stream(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("journal stream should start")
        .collect()
        .await
}

#[tokio::test(flavor = "current_thread")]
async fn assistant_output_projects_to_public_assistant_message() {
    let provider = FakeModelProvider::new(vec![Ok(completed_text_event("hello public event"))]);
    let runtime = Runtime::builder(session_id("public-assistant-message"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_public_stream(&runtime, "Say hello.").await;

    assert!(matches!(events[0], RuntimeEvent::SessionStarted { .. }));
    assert!(matches!(events[1], RuntimeEvent::StepStarted { .. }));
    let RuntimeEvent::AssistantMessage {
        text,
        artifact,
        source,
    } = &events[2]
    else {
        panic!("expected assistant message, got {:?}", events[2]);
    };
    assert_eq!(text, "hello public event");
    assert_eq!(artifact.kind(), &ArtifactKind::Text);
    assert_eq!(source.sequence, 2);
    assert!(matches!(events[3], RuntimeEvent::StepCompleted { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_pending_projects_to_public_started_event() {
    let provider = FakeModelProvider::new(vec![Ok(completed_tool_call_event(model_tool_call(
        "call-public-tool",
        "lookup",
    )))]);
    let runtime = Runtime::builder(session_id("public-tool-started"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    let events = collect_public_stream(&runtime, "Call lookup.").await;

    assert!(matches!(
        events.iter().find(|event| matches!(event, RuntimeEvent::ToolCallStarted { .. })),
        Some(RuntimeEvent::ToolCallStarted { call, .. })
            if call.id().as_str() == "call-public-tool" && call.name().as_str() == "lookup"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn bridge_tool_public_stream_exposes_only_ordinary_tool_started_event() {
    let provider = FakeModelProvider::new(vec![Ok(completed_tool_call_event(model_tool_call(
        "call-bridge-public",
        "bridge_lookup",
    )))]);
    let runtime = Runtime::builder(session_id("public-bridge-tool"))
        .allow_bridge_tools()
        .register_tool(RegisteredTool::bridge(tool_spec("bridge_lookup")))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_public_stream(&runtime, "Call bridge tool.").await;
    let json = serde_json::to_value(&events).expect("events should serialize");

    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCallStarted { call, .. }
            if call.id().as_str() == "call-bridge-public"
    )));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::ToolCallStarted { .. }))
            .count(),
        1
    );
    let text = json.to_string();
    assert!(!text.contains("bridge_tool_call_requested"));
    assert!(!text.contains("runner"));
}

#[tokio::test(flavor = "current_thread")]
async fn stream_and_journal_stream_are_separate_surfaces() {
    let provider = FakeModelProvider::new(vec![Ok(completed_text_event("journal answer"))]);
    let runtime = Runtime::builder(session_id("public-journal-separate"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let journal_events = collect_journal_stream(&runtime, "Raw journal.").await;

    assert!(matches!(
        journal_events[2].payload,
        RuntimeJournalPayload::AssistantOutputRecorded { .. }
    ));
}
