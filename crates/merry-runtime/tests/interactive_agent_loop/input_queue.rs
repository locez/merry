use crate::support::{
    models::{BlockingFirstProvider, RecordingProvider, model_name},
    runtime::session_id,
};
use merry_core::{InteractiveRunState, QueuedInputLane, RuntimeEvent};
use merry_llm::ModelMessageRole;
use merry_runtime::{AgentLoopConfig, InterruptReason, Runtime, StepContext};
use std::sync::Arc;
use tokio::{
    sync::oneshot,
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn enqueue_while_waiting_starts_backlog_turn() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-backlog-waiting"))
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
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    let item = input.enqueue("later").await.expect("backlog queued");
    assert_eq!(item.lane(), QueuedInputLane::Backlog);
    assert_eq!(item.text(), "later");

    let mut saw_accepted = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if matches!(
            event,
            RuntimeEvent::QueuedInputAccepted {
                lane: QueuedInputLane::Backlog,
                ..
            }
        ) {
            saw_accepted = true;
            break;
        }
    }
    assert!(saw_accepted);
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test]
async fn next_burst_before_boundary_becomes_two_user_messages_in_one_request() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-next-burst"))
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
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    let first = input.submit_next("first");
    let second = input.submit_next("second");
    let (first, second) = timeout(Duration::from_millis(200), async move {
        tokio::join!(first, second)
    })
    .await
    .expect("running step should keep accepting queued next input");
    let first = first.expect("first queued");
    let second = second.expect("second queued");

    release_tx.send(()).expect("first provider step released");

    let mut saw_two = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Next,
        } = event
            && inputs
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>()
                == vec![first.text(), second.text()]
        {
            saw_two = true;
            break;
        }
    }
    assert!(saw_two);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let user_texts = requests[1]
        .messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::User)
        .map(|message| message.content().as_text().to_owned())
        .collect::<Vec<_>>();
    assert!(user_texts.ends_with(&["first".to_owned(), "second".to_owned()]));
}

#[tokio::test]
async fn next_burst_does_not_reorder_backlog() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-backlog-order"))
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
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    input.enqueue("backlog").await.expect("backlog queued");
    input.submit_next("next").await.expect("next queued");

    release_tx.send(()).expect("first provider step released");

    let mut accepted = Vec::new();
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted { inputs, .. } = event {
            accepted.extend(
                inputs
                    .into_iter()
                    .map(|item| item.text)
                    .filter(|text| text == "next" || text == "backlog"),
            );
            if accepted.contains(&"next".to_owned()) {
                break;
            }
        }
    }
    assert_eq!(accepted, vec!["next".to_owned()]);

    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Backlog,
        } = event
        {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["backlog"]
            );
            break;
        }
    }
}

#[tokio::test]
async fn input_handle_updates_removes_and_reorders_pending_items() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-edit-queue"))
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
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    let mut first = input.enqueue("first").await.expect("first queued");
    let second = input.enqueue("second").await.expect("second queued");

    first
        .update("updated")
        .await
        .expect("pending input updates");
    let mut snapshot = input.snapshot().await.expect("snapshot");
    snapshot.backlog.swap(0, 1);
    input
        .replace_pending_order(QueuedInputLane::Backlog, &snapshot.backlog)
        .await
        .expect("pending input reorders");
    second.remove().await.expect("pending input removes");

    let snapshot = input.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.backlog.len(), 1);
    assert_eq!(snapshot.backlog[0].text(), "updated");

    release_tx.send(()).expect("first provider step released");

    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Backlog,
        } = event
        {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["updated"]
            );
            break;
        }
    }
}

#[tokio::test]
async fn interrupt_moves_existing_next_to_suspended_and_post_interrupt_next_runs_alone() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-esc-suspended"))
        .model_provider(Arc::new(provider.clone()), model_name())
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

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("initial provider step starts");
    input.submit_next("x").await.expect("x queued");
    input.submit_next("y").await.expect("y queued");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");

    let snapshot = input.snapshot().await.expect("snapshot");
    assert_eq!(
        snapshot
            .suspended
            .iter()
            .map(|item| item.text())
            .collect::<Vec<_>>(),
        vec!["x", "y"]
    );

    input.submit_next("z").await.expect("z queued");
    drop(release_tx);

    let mut saw_z = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Next,
        } = event
            && inputs.iter().any(|item| item.text == "z")
        {
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].text, "z");
            saw_z = true;
            break;
        }
    }
    assert!(saw_z);
}

#[tokio::test]
async fn resume_suspended_accepts_suspended_burst_when_waiting() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-resume-suspended"))
        .model_provider(Arc::new(provider.clone()), model_name())
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

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("initial provider step starts");
    input.submit_next("suspended").await.expect("queued");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");
    drop(release_tx);

    let mut waiting = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if matches!(
            event,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput
            }
        ) {
            waiting = true;
            break;
        }
    }
    assert!(waiting);

    control.resume_suspended().await.expect("suspended resumes");

    let mut saw_suspended = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Suspended,
        } = event
        {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["suspended"]
            );
            saw_suspended = true;
            break;
        }
    }
    assert!(saw_suspended);
}
