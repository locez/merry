use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, RuntimeEvent, RuntimeEventKind,
    SessionId,
};
use merry_runtime::{
    ArtifactContent, ContextCompiler, ContextSummary, Runtime, StepContext, StepInput,
};
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

#[tokio::test(flavor = "current_thread")]
async fn first_step_emits_session_started_then_step_lifecycle() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "hello").await;

    assert_eq!(events.len(), 3);
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
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
    assert_eq!(
        second_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(matches!(
        second_events[0].kind,
        RuntimeEventKind::StepStarted
    ));
    assert!(matches!(
        second_events[1].kind,
        RuntimeEventKind::StepCompleted
    ));
}
