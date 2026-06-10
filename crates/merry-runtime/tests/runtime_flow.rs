use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, RuntimeJournalEvent,
    RuntimeJournalPayload, SessionId, ToolCallId, ToolCallResult, ToolInputSchema, ToolName,
    ToolSpec,
};
use merry_runtime::{
    ArtifactContent, ArtifactContentKind, ArtifactError, ContextCompiler, ContextSummary,
    LedgerFactKind, LedgerProjection, RegisteredTool, Runtime, RuntimeError, StepContext,
    StepInput, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use schemars::Schema;
use serde_json::json;
use std::{num::NonZeroUsize, sync::Arc};
use tokio_util::sync::CancellationToken;

fn session_id() -> SessionId {
    SessionId::new("test-session").expect("valid session id")
}

fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("valid artifact id")
}

fn tool_call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("valid tool call id")
}

fn tool_result(call_id: &str, artifact: &str, kind: ArtifactKind) -> ToolCallResult {
    ToolCallResult::succeeded(
        tool_call_id(call_id),
        ArtifactRef::new(artifact_id(artifact), kind),
    )
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
        Ok(_) => panic!("duplicate profile tool names should be rejected"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_records_context_state_and_builds_compilable_snapshot() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");
    runtime
        .record_artifact(
            ArtifactRef::new(artifact_id("runtime-flow-artifact"), ArtifactKind::Text),
            ArtifactContent::text("alpha\nbeta\ngamma\n"),
        )
        .await
        .expect("artifact should record");
    let evidence = runtime
        .evidence_ref(
            &artifact_id("runtime-flow-artifact"),
            EvidenceLocator::line_range(2, 3).expect("valid line range"),
        )
        .await
        .expect("evidence should resolve through session artifacts");

    runtime
        .record_context_summary(
            ContextSummary::new(
                "runtime-flow-summary",
                "Runtime facade owns context state.",
                vec![
                    merry_runtime::ContextEvidence::new("runtime evidence", evidence)
                        .expect("valid context evidence"),
                ],
            )
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
            "evidence:runtime evidence:runtime-flow-artifact:line:2-3",
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
    let result = tool_result("unknown-call", "unknown-tool-result", ArtifactKind::Text);
    let before = runtime.ledger_projection().await;

    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::text("not recorded\n"))
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
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("unknown result artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == *result.artifact().id()
    ));

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

    let result = tool_result(
        "call-while-active",
        "active-step-result",
        ArtifactKind::Text,
    );
    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::text("should not record\n"))
        .await
        .expect_err("tool result submission should be rejected during active step");

    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id()
    ));
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("rejected result artifact must not be readable");
    assert!(matches!(
        evidence_err,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == *result.artifact().id()
    ));

    drop(stream);
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_reserved_id_keeps_step_already_active_priority() {
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

    let result = tool_result(
        "call-while-active",
        "tool-result-active",
        ArtifactKind::Text,
    );
    let err = runtime
        .submit_tool_result(
            result,
            ArtifactContent::text("should not reach reserved validation\n"),
        )
        .await
        .expect_err("active step should be checked before reserved artifact id");

    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id()
    ));

    drop(stream);
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
async fn record_artifact_returns_session_started_then_artifact_recorded() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");
    let artifact = ArtifactRef::new(artifact_id("eventful-artifact"), ArtifactKind::Text);

    let events = runtime
        .record_artifact(artifact.clone(), ArtifactContent::text("exact evidence\n"))
        .await
        .expect("artifact should record with events");

    assert_eq!(events.len(), 2);
    assert_sequences(&events, &[0, 1]);
    assert!(matches!(
        events[0].payload,
        RuntimeJournalPayload::SessionStarted
    ));
    assert!(matches!(
        &events[1].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact: recorded } if recorded == &artifact
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn record_artifact_after_step_completion_continues_global_sequence() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");
    let step_events = collect_step(&runtime, "before artifact").await;
    assert_sequences(&step_events, &[0, 1, 2]);

    let artifact = ArtifactRef::new(artifact_id("post-step-artifact"), ArtifactKind::Text);
    let events = runtime
        .record_artifact(
            artifact.clone(),
            ArtifactContent::text("post-step evidence\n"),
        )
        .await
        .expect("artifact should record after step completes");

    assert_eq!(events.len(), 1);
    assert_sequences(&events, &[3]);
    assert!(matches!(
        &events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact: recorded } if recorded == &artifact
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn record_artifact_is_rejected_while_step_is_active() {
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

    let artifact = ArtifactRef::new(artifact_id("active-step-artifact"), ArtifactKind::Text);
    let err = runtime
        .record_artifact(
            artifact.clone(),
            ArtifactContent::text("should not record\n"),
        )
        .await
        .expect_err("artifact recording should be rejected during active step");

    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id()
    ));
    let evidence_err = runtime
        .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("rejected artifact must not be readable");
    assert!(matches!(
        evidence_err,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == *artifact.id()
    ));

    drop(stream);
}

#[tokio::test(flavor = "current_thread")]
async fn record_artifact_rejects_reserved_assistant_output_id_without_mutation() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");
    for (reserved_id, artifact_kind, content) in [
        (
            "assistant-output-3",
            ArtifactKind::Text,
            ArtifactContent::text("external shadow output\n"),
        ),
        (
            "process-input-3",
            ArtifactKind::Json,
            ArtifactContent::json(r#"{"kind":"external-shadow"}"#),
        ),
    ] {
        let artifact = ArtifactRef::new(artifact_id(reserved_id), artifact_kind);
        let before = runtime.ledger_projection().await;

        let err = runtime
            .record_artifact(artifact.clone(), content)
            .await
            .expect_err("external recording should not use runtime-owned artifact ids");
        let after = runtime.ledger_projection().await;

        assert!(matches!(
            err,
            RuntimeError::ReservedArtifactId { artifact_id } if artifact_id == *artifact.id()
        ));
        assert_projection_unchanged(&before, &after);
        let evidence_err = runtime
            .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
            .await
            .expect_err("reserved artifact must not be recorded");
        assert!(matches!(
            evidence_err,
            RuntimeError::Artifact {
                source: ArtifactError::MissingArtifact { id }
            } if id == *artifact.id()
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn record_artifact_reserved_id_keeps_step_already_active_priority() {
    let runtime = Runtime::builder(session_id())
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero buffer"))
        .build()
        .expect("runtime should build");
    let stream = runtime
        .step(
            StepInput::user_text("hold active step").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("step should start");
    tokio::task::yield_now().await;

    let err = runtime
        .record_artifact(
            ArtifactRef::new(artifact_id("assistant-output-3"), ArtifactKind::Text),
            ArtifactContent::text("should not reach reserved validation\n"),
        )
        .await
        .expect_err("active step should be checked before reserved artifact id");

    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id()
    ));

    drop(stream);
}

#[tokio::test(flavor = "current_thread")]
async fn artifact_event_is_returned_after_recorded_artifact_supports_evidence_refs() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");
    let artifact = ArtifactRef::new(artifact_id("event-evidence-artifact"), ArtifactKind::Text);

    let events = runtime
        .record_artifact(artifact.clone(), ArtifactContent::text("alpha\nbeta\n"))
        .await
        .expect("artifact should record with events");
    let evidence = runtime
        .evidence_ref(
            artifact.id(),
            EvidenceLocator::line_range(2, 2).expect("valid locator"),
        )
        .await
        .expect("artifact event should only be observable after evidence refs work");

    assert_eq!(evidence.artifact_id, *artifact.id());
    assert!(matches!(
        events.last().map(|event| &event.payload),
        Some(RuntimeJournalPayload::ArtifactRecorded { artifact: recorded }) if recorded == &artifact
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_artifact_event_recording_does_not_advance_sequence_or_projection() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");
    let artifact = ArtifactRef::new(artifact_id("duplicate-event-artifact"), ArtifactKind::Text);
    let initial_events = runtime
        .record_artifact(artifact.clone(), ArtifactContent::text("first\n"))
        .await
        .expect("initial artifact should record");
    let initial_projection = runtime.ledger_projection().await;

    let err = runtime
        .record_artifact(artifact.clone(), ArtifactContent::text("second\n"))
        .await
        .expect_err("duplicate artifact id should be rejected");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        RuntimeError::Artifact {
            source: ArtifactError::DuplicateId { id }
        } if id == *artifact.id()
    ));
    assert_eq!(initial_projection, projection_after_error);
    assert_eq!(initial_events.last().expect("artifact event").sequence, 1);

    let later_events = collect_step(&runtime, "after duplicate").await;
    assert_sequences(&later_events, &[2, 3]);
}

#[tokio::test(flavor = "current_thread")]
async fn incompatible_artifact_event_recording_does_not_advance_sequence_or_projection() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");

    let err = runtime
        .record_artifact(
            ArtifactRef::new(artifact_id("binary-marked-text"), ArtifactKind::Binary),
            ArtifactContent::text("not binary\n"),
        )
        .await
        .expect_err("incompatible artifact content should be rejected");
    let projection = runtime.ledger_projection().await;
    let events = collect_step(&runtime, "after incompatible").await;

    assert!(matches!(
        err,
        RuntimeError::Artifact {
            source: ArtifactError::IncompatibleContent {
                id,
                artifact_kind,
                content_kind
            }
        } if id == artifact_id("binary-marked-text")
            && artifact_kind == ArtifactKind::Binary
            && content_kind == ArtifactContentKind::Text
    ));
    assert!(projection.entries().is_empty());
    assert_sequences(&events, &[0, 1, 2]);
}

#[tokio::test(flavor = "current_thread")]
async fn ledger_projection_records_artifact_lifecycle_fact_at_event_sequence() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");
    let artifact = ArtifactRef::new(artifact_id("ledger-artifact"), ArtifactKind::Text);

    let events = runtime
        .record_artifact(artifact, ArtifactContent::text("ledger evidence\n"))
        .await
        .expect("artifact should record with events");
    let projection = runtime.ledger_projection().await;

    assert_sequences(&events, &[0, 1]);
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
                kind: LedgerFactKind::ArtifactRecorded,
            },
        ]
    );
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
