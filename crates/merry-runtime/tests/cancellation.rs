use futures_util::StreamExt;
use merry_core::{RuntimeEventKind, SessionId};
use merry_runtime::{Runtime, RuntimeError, StepContext, StepInput};
use std::num::NonZeroUsize;
use tokio_util::sync::CancellationToken;

fn session_id() -> SessionId {
    SessionId::new("cancel-session").expect("valid session id")
}

#[tokio::test(flavor = "current_thread")]
async fn pre_cancelled_step_emits_only_cancelled() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");
    let token = CancellationToken::new();
    token.cancel();

    let events = runtime
        .step(
            StepInput::user_text("hello").expect("valid step input"),
            StepContext::new(token),
        )
        .expect("pre-cancelled step should return a stream")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sequence, 0);
    match &events[0].kind {
        RuntimeEventKind::Cancelled { diagnostic } => {
            assert_eq!(diagnostic.code(), "cancelled");
        }
        other => panic!("expected Cancelled event, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_step_is_rejected() {
    let runtime = Runtime::builder(session_id())
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero buffer"))
        .build()
        .expect("runtime should build");

    let first_stream = runtime
        .step(
            StepInput::user_text("first").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("first step should start");

    let err = match runtime.step(
        StepInput::user_text("second").expect("valid step input"),
        StepContext::new(CancellationToken::new()),
    ) {
        Ok(_) => panic!("second step unexpectedly started"),
        Err(err) => err,
    };

    assert!(matches!(err, RuntimeError::StepAlreadyActive { .. }));
    drop(first_stream);
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_full_stream_releases_active_step_after_producer_stops() {
    let runtime = Runtime::builder(session_id())
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero buffer"))
        .build()
        .expect("runtime should build");
    let token = CancellationToken::new();

    let mut first_stream = runtime
        .step(
            StepInput::user_text("first").expect("valid step input"),
            StepContext::new(token.clone()),
        )
        .expect("first step should start");
    tokio::task::yield_now().await;

    let err = match runtime.step(
        StepInput::user_text("second").expect("valid step input"),
        StepContext::new(CancellationToken::new()),
    ) {
        Ok(_) => panic!("second step unexpectedly started while first producer is active"),
        Err(err) => err,
    };
    assert!(matches!(err, RuntimeError::StepAlreadyActive { .. }));

    token.cancel();
    let old_events = first_stream.by_ref().collect::<Vec<_>>().await;
    assert_eq!(old_events.len(), 1);
    assert!(matches!(
        old_events[0].kind,
        RuntimeEventKind::SessionStarted
    ));

    let second_events = runtime
        .step(
            StepInput::user_text("second").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("second step should start after cancellation cleanup")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(
        second_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
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

async fn start_step_after_cleanup(runtime: &Runtime, text: &str) -> Vec<merry_core::RuntimeEvent> {
    for _ in 0..8 {
        tokio::task::yield_now().await;

        match runtime.step(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        ) {
            Ok(stream) => return stream.collect::<Vec<_>>().await,
            Err(RuntimeError::StepAlreadyActive { .. }) => continue,
            Err(err) => panic!("unexpected step error after cleanup: {err}"),
        }
    }

    panic!("producer did not release active step after cancellation cleanup");
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_full_stream_keeps_step_active_until_producer_stops() {
    let runtime = Runtime::builder(session_id())
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero buffer"))
        .build()
        .expect("runtime should build");

    let first_stream = runtime
        .step(
            StepInput::user_text("first").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("first step should start");
    tokio::task::yield_now().await;
    drop(first_stream);

    let err = match runtime.step(
        StepInput::user_text("second").expect("valid step input"),
        StepContext::new(CancellationToken::new()),
    ) {
        Ok(_) => panic!("second step started before dropped producer stopped"),
        Err(err) => err,
    };
    assert!(matches!(err, RuntimeError::StepAlreadyActive { .. }));

    let second_events = start_step_after_cleanup(&runtime, "second").await;

    assert_eq!(
        second_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
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
