use futures_util::StreamExt;
use merry_core::{RuntimeEvent, RuntimeEventKind, SessionId};
use merry_runtime::{Runtime, StepContext, StepInput};
use tokio_util::sync::CancellationToken;

fn session_id() -> SessionId {
    SessionId::new("test-session").expect("valid session id")
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
