use crate::support::{
    models::{BlockingFirstProvider, RecordingProvider, model_name},
    runtime::session_id,
};
use merry_core::{InteractiveRunState, RuntimeEvent};
use merry_llm::ModelMessageRole;
use merry_runtime::{AgentLoopConfig, InterruptReason, Runtime, StepContext};
use std::sync::Arc;
use tokio::{
    sync::oneshot,
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn submit_while_waiting_starts_turn_immediately() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-submit-waiting"))
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

    let item = input.submit("later").await.expect("input queued");
    assert_eq!(item.text(), "later");

    let mut saw_accepted = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted { inputs } = event {
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].text, "later");
            saw_accepted = true;
            break;
        }
    }
    assert!(saw_accepted);
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test]
async fn pending_burst_before_boundary_becomes_two_user_messages_in_one_request() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-pending-burst"))
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

    input.submit("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    let first = input.submit("first");
    let second = input.submit("second");
    let (first, second) = timeout(Duration::from_millis(200), async move {
        tokio::join!(first, second)
    })
    .await
    .expect("running step should keep accepting pending input");
    let first = first.expect("first queued");
    let second = second.expect("second queued");

    release_tx.send(()).expect("first provider step released");

    let mut saw_two = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted { inputs } = event
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
async fn pausing_for_edit_blocks_auto_dispatch_until_resumed_then_flushes_all_together() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-pause-for-edit"))
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

    input.submit("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    let mut first = input.submit("draft one").await.expect("first queued");
    input.submit("draft two").await.expect("second queued");

    // User starts editing item 1 in the UI -> runtime pauses auto-dispatch.
    input.set_paused(true).await.expect("pause succeeds");

    // First model turn completes while user is still editing.
    release_tx.send(()).expect("first provider step released");

    // Loop must settle to WaitingForInput without sending either pending item!
    let mut reached_waiting = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted { ref inputs } = event
            && inputs.iter().any(|item| item.text != "initial")
        {
            panic!("paused pending items must not auto-dispatch: {inputs:?}");
        }
        if matches!(
            event,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput
            }
        ) {
            reached_waiting = true;
            break;
        }
    }
    assert!(reached_waiting);
    assert_eq!(provider.recorded_requests().len(), 1);

    // User finishes editing and resumes -> all pending items flush together in one burst.
    first
        .update("edited one")
        .await
        .expect("pending edit succeeds");
    input.set_paused(false).await.expect("resume succeeds");

    let mut saw_burst = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted { inputs } = event {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["edited one", "draft two"]
            );
            saw_burst = true;
            break;
        }
    }
    assert!(saw_burst);
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

    input.submit("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    let mut first = input.submit("first").await.expect("first queued");
    let second = input.submit("second").await.expect("second queued");

    first
        .update("updated")
        .await
        .expect("pending input updates");
    let mut snapshot = input.snapshot().await.expect("snapshot");
    snapshot.items.swap(0, 1);
    input
        .replace_pending_order(&snapshot.items)
        .await
        .expect("pending input reorders");
    second.remove().await.expect("pending input removes");

    let snapshot = input.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].text(), "updated");

    release_tx.send(()).expect("first provider step released");

    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted { inputs } = event
            && inputs.iter().any(|item| item.text == "updated")
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
async fn interrupt_pauses_pending_items_until_explicitly_flushed() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-esc-pauses"))
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

    input.submit("initial").await.expect("initial queued");
    started_rx.await.expect("initial provider step starts");
    input.submit("x").await.expect("x queued");
    input.submit("y").await.expect("y queued");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");

    let snapshot = input.snapshot().await.expect("snapshot");
    assert!(snapshot.paused);
    assert_eq!(
        snapshot
            .items
            .iter()
            .map(|item| item.text())
            .collect::<Vec<_>>(),
        vec!["x", "y"]
    );

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

    // Submitting another item while paused adds to the pending queue without firing yet.
    input.submit("z").await.expect("z queued");
    let snapshot = input.snapshot().await.expect("snapshot");
    assert!(snapshot.paused);
    assert_eq!(snapshot.items.len(), 3);

    // Flushing sends all pending items together.
    input.flush().await.expect("flush succeeds");

    let mut saw_all = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::QueuedInputAccepted { inputs } = event {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["x", "y", "z"]
            );
            saw_all = true;
            break;
        }
    }
    assert!(saw_all);
}
