use crate::support::{
    models::{BlockingFirstProvider, RecordingProvider, model_name},
    runtime::{session_id, wait_for_interactive_waiting},
};
use merry_core::{InteractiveRunState, RuntimeEvent};
use merry_runtime::{
    AgentLoopConfig, FileSessionStore, InteractiveError, Runtime, SessionTranscriptItem,
    StepContext,
};
use std::sync::Arc;
use tokio::{
    sync::oneshot,
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn interactive_run_starts_waiting_for_input() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-waiting"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, _input, _control) = run.split();

    let event = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("state event");
    assert!(matches!(
        event,
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));
    assert!(provider.recorded_requests().is_empty());
}

#[tokio::test]
async fn interactive_control_saves_at_a_waiting_boundary_without_closing_the_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let id = session_id("interactive-save-waiting");
    let runtime = Runtime::builder(id.clone())
        .model_provider(Arc::new(RecordingProvider::new()), model_name())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    assert!(matches!(
        stream
            .next_event()
            .await
            .expect("stream error")
            .expect("waiting state"),
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));
    input
        .submit("persist this turn")
        .await
        .expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;

    control
        .save_session_to(store.clone())
        .await
        .expect("waiting save succeeds");
    let resumed = Runtime::builder(id)
        .resume_from_store_without_automatic_savepoints(store)
        .await
        .expect("saved session resumes");
    assert_eq!(
        resumed
            .session_transcript()
            .await
            .expect("transcript reads"),
        vec![
            SessionTranscriptItem::UserMessage {
                text: "persist this turn".to_owned(),
                images: Vec::new(),
            },
            SessionTranscriptItem::AssistantText {
                text: "done".to_owned(),
            },
        ]
    );

    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream should close");
}

#[tokio::test]
async fn interactive_control_returns_waiting_save_failures_through_the_ack() {
    let temp = tempfile::tempdir().expect("tempdir");
    let blocked_root = temp.path().join("not-a-directory");
    std::fs::write(&blocked_root, "blocked").expect("blocking file");
    let runtime = Runtime::builder(session_id("interactive-save-error"))
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, _input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    let error = control
        .save_session_to(FileSessionStore::new(blocked_root))
        .await
        .expect_err("write failure reaches the caller");
    assert!(matches!(error, InteractiveError::Runtime { .. }));

    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream should close");
}

#[tokio::test]
async fn submit_while_waiting_starts_model_turn() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-submit-next"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    assert!(matches!(
        stream
            .next_event()
            .await
            .expect("stream error")
            .expect("state event"),
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));

    let item = input.submit("hello").await.expect("input queued");
    assert_eq!(item.text(), "hello");

    let mut saw_accepted = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if matches!(event, RuntimeEvent::QueuedInputAccepted { .. }) {
            saw_accepted = true;
            break;
        }
    }
    assert!(saw_accepted);
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test]
async fn close_during_running_model_reaches_closed_event() {
    let (started_tx, started_rx) = oneshot::channel();
    let (_release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-close-running"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");
    input.submit("initial").await.expect("initial queued");
    started_rx.await.expect("provider step starts");

    control.close().await.expect("close accepted");

    timeout(Duration::from_secs(1), async {
        while let Some(event) = stream.next_event().await.expect("interactive stream error") {
            if matches!(event, RuntimeEvent::Closed) {
                return;
            }
        }
        panic!("stream ended before closed event");
    })
    .await
    .expect("close while running should reach closed event");
}

#[tokio::test]
async fn interactive_control_compacts_at_waiting_boundary_and_passes_focus() {
    let provider = RecordingProvider::new();
    let policy = merry_runtime::CitationCompactionPolicy::new(Some(128), Some(4096), 1)
        .expect("valid compaction policy");
    let runtime = Runtime::builder(session_id("interactive-compact-waiting"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .automatic_compaction(merry_runtime::CompactionConfig::enabled(policy))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("initial waiting state");

    input.submit("turn one").await.expect("turn one queued");
    wait_for_interactive_waiting(&mut stream).await;
    input.submit("turn two").await.expect("turn two queued");
    wait_for_interactive_waiting(&mut stream).await;

    let compact_control = control.clone();
    let compact_task = tokio::spawn(async move {
        compact_control
            .compact_context(Some("preserve auth invariant".to_owned()))
            .await
    });

    let mut saw_started = false;
    let mut saw_completed = false;
    while let Some(event) = stream.next_event().await.expect("stream error") {
        match event {
            RuntimeEvent::CompactionStarted { .. } => saw_started = true,
            RuntimeEvent::CompactionCompleted { .. } => saw_completed = true,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            } if saw_completed => break,
            _ => {}
        }
    }

    let outcome = compact_task
        .await
        .expect("compact task joins")
        .expect("manual compaction succeeds")
        .expect("history should compact");
    assert!(saw_started);
    assert!(saw_completed);
    assert!(outcome.covered_model_turn_count() >= 1);

    let requests = provider.recorded_requests();
    let compact_request = requests.last().expect("compaction request recorded");
    let last_text = compact_request
        .messages()
        .last()
        .expect("directive message")
        .content()
        .as_text();
    assert!(
        last_text.contains("preserve auth invariant"),
        "compaction request should include user focus: {last_text}"
    );

    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream should close");
}

#[tokio::test]
async fn interactive_control_compact_returns_none_when_nothing_to_compact() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-compact-empty"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, _input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("initial waiting state");

    let compact_control = control.clone();
    let compact_task = tokio::spawn(async move { compact_control.compact_context(None).await });

    let mut saw_started = false;
    while let Some(event) = stream.next_event().await.expect("stream error") {
        match event {
            RuntimeEvent::CompactionStarted { .. } => saw_started = true,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            } => break,
            _ => {}
        }
    }

    let outcome = compact_task
        .await
        .expect("compact task joins")
        .expect("empty compaction returns Ok(None)");
    assert!(outcome.is_none());
    assert!(!saw_started);
    assert!(provider.recorded_requests().is_empty());

    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream should close");
}

#[tokio::test]
async fn interactive_control_compact_rejects_while_model_turn_is_active() {
    let (started_tx, started_rx) = oneshot::channel();
    let (_release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-compact-running"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("initial waiting state");
    input.submit("initial").await.expect("initial queued");
    started_rx.await.expect("provider step starts");

    let error = control
        .compact_context(None)
        .await
        .expect_err("compaction while running should reject");
    assert!(matches!(error, InteractiveError::CompactRequiresIdle));

    control.close().await.expect("close accepted");
    stream
        .wait_until_closed()
        .await
        .expect("stream should close");
}
