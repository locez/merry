use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, RuntimeEvent, RuntimeEventKind,
    SessionId,
};
use merry_runtime::{
    ArtifactContent, ArtifactContentKind, ArtifactError, ContextCompiler, ContextSummary,
    LedgerFactKind, LedgerProjection, Runtime, RuntimeError, StepContext, StepInput,
};
use std::num::NonZeroUsize;
use tokio_util::sync::CancellationToken;

fn session_id() -> SessionId {
    SessionId::new("test-session").expect("valid session id")
}

fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("valid artifact id")
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
        .await;

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

#[track_caller]
fn assert_sequences(events: &[RuntimeEvent], expected: &[u64]) {
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
    assert!(matches!(events[0].kind, RuntimeEventKind::SessionStarted));
    assert!(matches!(
        &events[1].kind,
        RuntimeEventKind::ArtifactRecorded { artifact: recorded } if recorded == &artifact
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
        &events[0].kind,
        RuntimeEventKind::ArtifactRecorded { artifact: recorded } if recorded == &artifact
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
        events.last().map(|event| &event.kind),
        Some(RuntimeEventKind::ArtifactRecorded { artifact: recorded }) if recorded == &artifact
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
    assert!(matches!(events[0].kind, RuntimeEventKind::SessionStarted));
    assert!(matches!(events[1].kind, RuntimeEventKind::StepStarted));
    assert!(matches!(events[2].kind, RuntimeEventKind::StepCompleted));
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
        second_events[0].kind,
        RuntimeEventKind::StepStarted
    ));
    assert!(matches!(
        second_events[1].kind,
        RuntimeEventKind::StepCompleted
    ));
}
