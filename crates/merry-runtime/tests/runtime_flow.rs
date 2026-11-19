use futures_util::StreamExt;
use merry_core::{
    RuntimeJournalEvent, RuntimeJournalPayload, SessionId, ToolCallId, ToolCallResult,
    ToolInputSchema, ToolName, ToolOutput, ToolSpec,
};
use merry_runtime::{
    ContextCompiler, ContextSummary, RegisteredTool, Runtime, RuntimeError, StepContext, StepInput,
    ToolExecutionContext, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use schemars::Schema;
use serde_json::json;
use std::{num::NonZeroUsize, sync::Arc};
use tokio_util::sync::CancellationToken;

fn session_id() -> SessionId {
    SessionId::new("test-session").expect("valid session id")
}

fn tool_call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("valid tool call id")
}

fn tool_result(call_id: &str) -> ToolCallResult {
    ToolCallResult::succeeded(tool_call_id(call_id))
}

fn tool_spec(name: &str) -> ToolSpec {
    let schema =
        Schema::try_from(json!({ "type": "object" })).expect("test schema should be a JSON schema");
    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Test tool",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

struct StaticToolExecutor;

impl ToolExecutor for StaticToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: merry_core::PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(ToolExecutionOutcome::succeeded_text("ok\n")) })
    }
}

async fn collect_step(runtime: &Runtime, text: &str) -> Vec<RuntimeJournalEvent> {
    runtime
        .step(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("step should start")
        .collect()
        .await
}

#[track_caller]
fn assert_projection_unchanged(
    before: &merry_runtime::LedgerProjectionSnapshot,
    after: &merry_runtime::LedgerProjectionSnapshot,
) {
    assert_eq!(before, after);
}

#[track_caller]
fn assert_sequences(events: &[RuntimeJournalEvent], expected: &[u64]) {
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        expected
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_builder_with_profile_applies_capabilities_context_and_tools() {
    let profile = merry_runtime::RuntimeProfile::builder()
        .capabilities(merry_runtime::RuntimeCapabilities::default().allow_network())
        .initial_context_summary(
            "profile-capabilities",
            "RuntimeProfile applies complete runtime shape.",
        )
        .register_tool(RegisteredTool::read_only(
            tool_spec("profile_tool"),
            Arc::new(StaticToolExecutor),
        ))
        .build()
        .expect("profile should build");

    let runtime = Runtime::builder(session_id())
        .with_profile(profile)
        .expect("profile should apply")
        .build()
        .expect("runtime should build");
    assert!(runtime.capabilities().network_allowed());

    let compiled = ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("context should compile");
    assert!(
        compiled
            .to_snapshot()
            .contains(&"summary:profile-capabilities".to_owned())
    );
}

#[test]
fn runtime_profile_rejects_duplicate_tool_names() {
    let result = merry_runtime::RuntimeProfile::builder()
        .register_tool(RegisteredTool::read_only(
            tool_spec("duplicate_profile_tool"),
            Arc::new(StaticToolExecutor),
        ))
        .register_tool(RegisteredTool::read_only(
            tool_spec("duplicate_profile_tool"),
            Arc::new(StaticToolExecutor),
        ))
        .build();

    match result {
        Err(merry_runtime::RuntimeProfileError::DuplicateToolRegistration { .. }) => {}
        Err(merry_runtime::RuntimeProfileError::ReservedToolName { .. }) => {
            panic!("the duplicate test should not use a reserved tool name")
        }
        Ok(_) => panic!("duplicate profile tool names should be rejected"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_records_context_state_and_builds_compilable_snapshot() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");

    runtime
        .record_context_summary(
            ContextSummary::new("runtime-flow-summary", "Runtime facade owns context state.")
                .expect("valid summary"),
        )
        .await
        .expect("context summary should record");

    let compiled = ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("context should compile from session snapshot");

    assert_eq!(
        compiled.to_snapshot(),
        [
            "summary:runtime-flow-summary",
            "text:Runtime facade owns context state.",
        ]
        .join("\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn new_runtime_has_no_pending_tool_calls() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");

    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_for_unknown_call_does_not_mutate_session() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");
    let result = tool_result("unknown-call");
    let before = runtime.ledger_projection().await;

    let err = runtime
        .submit_tool_result(result.clone(), ToolOutput::text("not recorded\n"))
        .await
        .expect_err("unknown tool call should be rejected");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        RuntimeError::UnknownToolCall {
            session_id: rejected_session,
            call_id
        } if rejected_session == session_id() && call_id == tool_call_id("unknown-call")
    ));
    assert_projection_unchanged(&before, &after);
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert!(
        runtime
            .read_tool_output(result.call_id())
            .await
            .expect("tool output query succeeds")
            .is_none()
    );

    let events = collect_step(&runtime, "after unknown submit").await;
    assert_sequences(&events, &[0, 1, 2]);
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_tool_registration_is_build_error() {
    let err = match Runtime::builder(session_id())
        .register_tool(RegisteredTool::read_only(
            tool_spec("duplicate_tool"),
            Arc::new(StaticToolExecutor),
        ))
        .register_tool(RegisteredTool::read_only(
            tool_spec("duplicate_tool"),
            Arc::new(StaticToolExecutor),
        ))
        .build()
    {
        Ok(_) => panic!("duplicate tool registration should fail"),
        Err(err) => err,
    };

    assert!(matches!(
        err,
        RuntimeError::DuplicateToolRegistration { name }
            if name == ToolName::new("duplicate_tool").expect("valid tool name")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_result_for_unknown_call_does_not_mutate_session() {
    let runtime = Runtime::builder(session_id())
        .register_tool(RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(StaticToolExecutor),
        ))
        .build()
        .expect("runtime should build");
    let before = runtime.ledger_projection().await;
    let unknown = tool_call_id("unknown-execute-call");

    let err = runtime
        .execute_tool_call(&unknown, ToolExecutionContext::default())
        .await
        .expect_err("unknown tool call should be rejected");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        RuntimeError::UnknownToolCall {
            session_id: rejected_session,
            call_id
        } if rejected_session == session_id() && call_id == unknown
    ));
    assert_projection_unchanged(&before, &after);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_is_rejected_while_step_is_active() {
    let runtime = Runtime::builder(session_id())
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero buffer"))
        .build()
        .expect("runtime should build");
    let stream = runtime
        .step(
            StepInput::user_text("hold the stream open").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("step should start");
    tokio::task::yield_now().await;

    let result = tool_result("call-while-active");
    let err = runtime
        .submit_tool_result(result.clone(), ToolOutput::text("should not record\n"))
        .await
        .expect_err("tool result submission should be rejected during active step");

    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id()
    ));
    assert!(
        runtime
            .read_tool_output(result.call_id())
            .await
            .expect("tool output query succeeds")
            .is_none()
    );

    drop(stream);
}

#[tokio::test(flavor = "current_thread")]
async fn first_step_emits_session_started_then_step_lifecycle() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "hello").await;

    assert_eq!(events.len(), 3);
    assert_sequences(&events, &[0, 1, 2]);
    assert!(matches!(
        events[0].payload,
        RuntimeJournalPayload::SessionStarted
    ));
    assert!(matches!(
        events[1].payload,
        RuntimeJournalPayload::StepStarted
    ));
    assert!(matches!(
        events[2].payload,
        RuntimeJournalPayload::StepCompleted
    ));
    assert!(events.iter().all(|event| event.session_id == session_id()));
}

#[tokio::test(flavor = "current_thread")]
async fn second_step_continues_sequence_without_restarting_session() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");

    let first_events = collect_step(&runtime, "first").await;
    let second_events = collect_step(&runtime, "second").await;

    assert_eq!(first_events.len(), 3);
    assert_eq!(second_events.len(), 2);
    assert_sequences(&second_events, &[3, 4]);
    assert!(matches!(
        second_events[0].payload,
        RuntimeJournalPayload::StepStarted
    ));
    assert!(matches!(
        second_events[1].payload,
        RuntimeJournalPayload::StepCompleted
    ));
}
