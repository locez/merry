use super::*;
use crate::{FinishReason, ModelOutput, ModelResponse, Usage};
use tokio::sync::oneshot;

fn progress(index: u64) -> Result<ModelEvent, ModelError> {
    Ok(ModelEvent::OutputProgress {
        progress: Some(ModelOutputProgress::new(
            index * 4,
            Duration::from_millis(index),
        )),
    })
}

fn completed() -> Result<ModelEvent, ModelError> {
    Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("done")],
            FinishReason::Stop,
            Some(Usage::new(1, 100_000)),
        ),
    })
}

async fn signal(receiver: oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(2), receiver)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn paused_consumer_does_not_retain_or_block_a_long_reasoning_stream() {
    let (finished, received) = oneshot::channel();
    let source = stream::iter((1..=100_000).map(progress)).chain(stream::once(async move {
        let _ = finished.send(());
        completed()
    }));
    let output = receive_model_stream(Box::pin(source), &CancellationToken::new());
    signal(received).await;
    let events: Vec<_> = output.collect().await;
    let samples: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Ok(ModelEvent::OutputProgress {
                progress: Some(progress),
            }) => Some(*progress),
            _ => None,
        })
        .collect();
    assert!(
        samples.len() <= 2,
        "intermediate observations must be replaced, not queued"
    );
    assert_eq!(
        samples.last(),
        Some(&ModelOutputProgress::new(400_000, Duration::from_secs(100)))
    );
    assert!(matches!(
        events.last(),
        Some(Ok(ModelEvent::Completed { .. }))
    ));
}

#[tokio::test]
async fn fast_consumer_still_receives_rate_limited_progress_and_the_final_sample() {
    let source = stream::iter((1..=2_000).map(progress).chain([completed()]));
    let events: Vec<_> = receive_model_stream(Box::pin(source), &CancellationToken::new())
        .collect()
        .await;
    let samples: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Ok(ModelEvent::OutputProgress {
                progress: Some(progress),
            }) => Some(*progress),
            _ => None,
        })
        .collect();
    assert!(samples.len() <= 22);
    assert_eq!(
        samples.last(),
        Some(&ModelOutputProgress::new(8_000, Duration::from_secs(2)))
    );
}

#[tokio::test]
async fn semantic_backpressure_is_bounded_and_marks_timing_without_losing_text() {
    let (full, received) = oneshot::channel();
    let mut full = Some(full);
    let source = stream::iter([progress(1)])
        .chain(stream::iter((0..EVENT_CAPACITY * 2).map(move |index| {
            if index == EVENT_CAPACITY {
                let _ = full.take().unwrap().send(());
            }
            Ok(ModelEvent::OutputTextDelta {
                delta: "x".to_owned(),
            })
        })))
        .chain(stream::iter([progress(2_000), completed()]));
    let output = receive_model_stream(Box::pin(source), &CancellationToken::new());
    signal(received).await;
    let events: Vec<_> = output.collect().await;
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            Ok(ModelEvent::OutputTextDelta { delta }) => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "x".repeat(EVENT_CAPACITY * 2));
    let sample = events
        .iter()
        .rev()
        .find_map(|event| match event {
            Ok(ModelEvent::OutputProgress {
                progress: Some(progress),
            }) => Some(*progress),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        sample.timing_quality(),
        OutputTimingQuality::ConsumerLimited
    );
    assert_eq!(sample.utf8_bytes(), 8_000);
}

struct PendingSource(Option<oneshot::Sender<()>>);

impl Stream for PendingSource {
    type Item = Result<ModelEvent, ModelError>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl Drop for PendingSource {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[tokio::test]
async fn dropping_the_stream_stops_its_source_without_cancelling_the_parent() {
    let token = CancellationToken::new();
    let (dropped, received) = oneshot::channel();
    let output = receive_model_stream(Box::pin(PendingSource(Some(dropped))), &token);
    drop(output);
    signal(received).await;
    assert!(!token.is_cancelled());
}

#[tokio::test]
async fn cancellation_awaits_source_cleanup_before_returning_the_terminal_error() {
    let token = CancellationToken::new();
    let (dropped, received) = oneshot::channel();
    let mut output = receive_model_stream(Box::pin(PendingSource(Some(dropped))), &token);
    token.cancel();
    assert!(matches!(
        output.next().await,
        Some(Err(ModelError::Cancelled))
    ));
    signal(received).await;
    assert!(output.next().await.is_none());
}

#[tokio::test]
async fn exceptional_worker_shutdown_is_reported_instead_of_silent_eof() {
    let source = stream::once(async { panic!("fixture receive failure") });
    let mut output = receive_model_stream(Box::pin(source), &CancellationToken::new());
    let error = output.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Other);
    assert!(output.next().await.is_none());
}

#[tokio::test]
async fn independent_attempts_do_not_reuse_progress_or_timing_quality() {
    for _ in 0..2 {
        let source = stream::iter([progress(1_000), completed()]);
        let events: Vec<_> = receive_model_stream(Box::pin(source), &CancellationToken::new())
            .collect()
            .await;
        assert!(events.iter().any(|event| matches!(event, Ok(ModelEvent::OutputProgress { progress: Some(progress) }) if progress.timing_quality() == OutputTimingQuality::ReceiveWindow)));
    }
}

#[tokio::test]
async fn receive_clock_is_sampled_before_a_delayed_consumer_resumes() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    let clock = Arc::new(AtomicU64::new(0));
    let receive_clock = Arc::clone(&clock);
    let (release, gate) = oneshot::channel();
    let (read, read_done) = oneshot::channel();
    let source = stream::iter([progress(0)])
        .chain(stream::once(async move {
            gate.await.unwrap();
            let observed = progress(receive_clock.load(Ordering::SeqCst));
            let _ = read.send(());
            observed
        }))
        .chain(stream::iter([completed()]));
    let output = receive_model_stream(Box::pin(source), &CancellationToken::new());
    clock.store(2_000, Ordering::SeqCst);
    release.send(()).unwrap();
    signal(read_done).await;
    clock.store(100_000, Ordering::SeqCst);
    let events: Vec<_> = output.collect().await;
    let last = events
        .iter()
        .rev()
        .find_map(|event| match event {
            Ok(ModelEvent::OutputProgress {
                progress: Some(progress),
            }) => Some(progress),
            _ => None,
        })
        .unwrap();
    assert_eq!(last.elapsed(), Duration::from_secs(2));
    assert_eq!(last.timing_quality(), OutputTimingQuality::ReceiveWindow);
}
