//! Owned receive task with bounded semantic delivery and replaceable telemetry.

use crate::{ModelError, ModelEvent, ModelEventStream, ModelOutputProgress, ProviderErrorKind};
use futures_core::Stream;
use futures_util::{StreamExt, stream};
use merry_core::OutputTimingQuality;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const EVENT_CAPACITY: usize = 32;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Starts an owned receive task for one provider attempt.
/// Semantic events remain ordered and bounded; live progress is latest-only and
/// published at most every 100 ms of observed output, plus resets and completion.
/// Slow semantic delivery marks timing as consumer-limited rather than reporting
/// a misleading rate. Completion awaits the task; dropping the stream cancels
/// and aborts it, dropping its input stream and network resources.
#[must_use]
pub fn receive_model_stream(
    source: ModelEventStream,
    token: &CancellationToken,
) -> ModelEventStream {
    let token = token.child_token();
    let (sender, receiver) = mpsc::channel(EVENT_CAPACITY);
    let (progress_sender, progress_receiver) = watch::channel(ProgressUpdate::default());
    let worker_token = token.clone();
    let worker = tokio::spawn(receive(source, sender, progress_sender, worker_token));
    let progress = stream::unfold(progress_receiver, |mut receiver| async move {
        receiver.changed().await.ok()?;
        let update = *receiver.borrow_and_update();
        Some((update, receiver))
    });
    Box::pin(ReceiveStream {
        receiver,
        progress: Box::pin(progress.fuse()),
        worker: Some(worker),
        token,
        last_revision: 0,
        prefer_progress: false,
        terminal: None,
        closed: false,
        done: false,
    })
}

#[derive(Clone, Copy, Default)]
struct ProgressUpdate {
    revision: u64,
    progress: Option<ModelOutputProgress>,
}

enum Delivery {
    Event(Result<ModelEvent, ModelError>),
    FinalProgress(ProgressUpdate),
}

#[derive(Default)]
struct ProgressPublisher {
    latest: ProgressUpdate,
    last_published: Option<Duration>,
    consumer_limited: bool,
}

impl ProgressPublisher {
    fn observe(
        &mut self,
        progress: Option<ModelOutputProgress>,
        sender: &watch::Sender<ProgressUpdate>,
    ) {
        if progress.is_none() {
            self.consumer_limited = false;
        }
        self.latest.revision = self.latest.revision.saturating_add(1);
        self.latest.progress = progress.map(|progress| {
            if self.consumer_limited {
                progress.with_timing_quality(OutputTimingQuality::ConsumerLimited)
            } else {
                progress
            }
        });
        let elapsed = progress.map(ModelOutputProgress::elapsed);
        if elapsed.is_none_or(|elapsed| {
            self.last_published
                .is_none_or(|last| elapsed.saturating_sub(last) >= PROGRESS_INTERVAL)
        }) {
            self.last_published = elapsed;
            sender.send_replace(self.latest);
        }
    }

    fn mark_consumer_limited(&mut self, sender: &watch::Sender<ProgressUpdate>) {
        if !self.consumer_limited {
            self.consumer_limited = true;
            if let Some(progress) = self.latest.progress {
                self.latest.revision = self.latest.revision.saturating_add(1);
                self.latest.progress =
                    Some(progress.with_timing_quality(OutputTimingQuality::ConsumerLimited));
                sender.send_replace(self.latest);
            }
        }
    }
}

async fn receive(
    mut source: ModelEventStream,
    sender: mpsc::Sender<Delivery>,
    progress_sender: watch::Sender<ProgressUpdate>,
    token: CancellationToken,
) -> Result<(), ModelError> {
    let mut progress = ProgressPublisher::default();
    loop {
        tokio::task::consume_budget().await;
        let item = tokio::select! {
            biased;
            () = token.cancelled() => return Err(ModelError::Cancelled),
            () = sender.closed() => return Ok(()),
            item = source.next() => item,
        };
        if let Some(Ok(ModelEvent::OutputProgress { progress: sample })) = item {
            progress.observe(sample, &progress_sender);
            continue;
        }
        let terminal = matches!(
            item,
            None | Some(Err(_)) | Some(Ok(ModelEvent::Completed { .. }))
        );
        if terminal && progress.latest.revision != 0 {
            progress_sender.send_replace(progress.latest);
            if !send(&sender, &token, Delivery::FinalProgress(progress.latest)).await? {
                return Ok(());
            }
        }
        let Some(item) = item else {
            return Ok(());
        };
        let delivery = Delivery::Event(item);
        if terminal {
            let _ = send(&sender, &token, delivery).await?;
            return Ok(());
        }
        match sender.try_send(delivery) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
            Err(mpsc::error::TrySendError::Full(delivery)) => {
                progress.mark_consumer_limited(&progress_sender);
                if !send(&sender, &token, delivery).await? {
                    return Ok(());
                }
            }
        }
    }
}

async fn send(
    sender: &mpsc::Sender<Delivery>,
    token: &CancellationToken,
    event: Delivery,
) -> Result<bool, ModelError> {
    tokio::select! {
        biased;
        () = token.cancelled() => Err(ModelError::Cancelled),
        result = sender.send(event) => Ok(result.is_ok()),
    }
}

struct ReceiveStream {
    receiver: mpsc::Receiver<Delivery>,
    progress: Pin<Box<dyn Stream<Item = ProgressUpdate> + Send>>,
    worker: Option<JoinHandle<Result<(), ModelError>>>,
    token: CancellationToken,
    last_revision: u64,
    prefer_progress: bool,
    terminal: Option<Result<ModelEvent, ModelError>>,
    closed: bool,
    done: bool,
}

impl ReceiveStream {
    fn observation(&mut self, update: ProgressUpdate) -> Option<Result<ModelEvent, ModelError>> {
        if update.revision <= self.last_revision {
            return None;
        }
        self.last_revision = update.revision;
        self.prefer_progress = false;
        Some(Ok(ModelEvent::OutputProgress {
            progress: update.progress,
        }))
    }

    fn poll_progress(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<ModelEvent, ModelError>>> {
        while let Poll::Ready(update) = self.progress.as_mut().poll_next(context) {
            let Some(update) = update else {
                return Poll::Ready(None);
            };
            if let Some(event) = self.observation(update) {
                return Poll::Ready(Some(event));
            }
        }
        Poll::Pending
    }

    fn poll_finish(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<ModelEvent, ModelError>>> {
        let Some(worker) = self.worker.as_mut() else {
            return Poll::Ready(None);
        };
        let result = std::task::ready!(Pin::new(worker).poll(context));
        self.worker = None;
        self.done = true;
        if self.token.is_cancelled() {
            return Poll::Ready(Some(Err(ModelError::Cancelled)));
        }
        Poll::Ready(match result {
            Ok(Ok(())) => self.terminal.take(),
            Ok(Err(error)) => Some(Err(error)),
            Err(_) => Some(Err(ModelError::provider(
                ProviderErrorKind::Other,
                "model stream receive task failed",
            ))),
        })
    }
}

impl Stream for ReceiveStream {
    type Item = Result<ModelEvent, ModelError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let state = self.get_mut();
        loop {
            if state.done {
                return Poll::Ready(None);
            }
            if state.token.is_cancelled() || state.closed || state.terminal.is_some() {
                return state.poll_finish(context);
            }
            if state.prefer_progress
                && let Poll::Ready(Some(event)) = state.poll_progress(context)
            {
                return Poll::Ready(Some(event));
            }
            match state.receiver.poll_recv(context) {
                Poll::Ready(Some(Delivery::Event(event))) => {
                    if matches!(event, Err(_) | Ok(ModelEvent::Completed { .. })) {
                        state.terminal = Some(event);
                        continue;
                    }
                    state.prefer_progress = true;
                    return Poll::Ready(Some(event));
                }
                Poll::Ready(Some(Delivery::FinalProgress(update))) => {
                    if let Some(event) = state.observation(update) {
                        return Poll::Ready(Some(event));
                    }
                }
                Poll::Ready(None) => state.closed = true,
                Poll::Pending => {
                    if let Poll::Ready(Some(event)) = state.poll_progress(context) {
                        return Poll::Ready(Some(event));
                    }
                    return Poll::Pending;
                }
            }
        }
    }
}

impl Drop for ReceiveStream {
    fn drop(&mut self) {
        self.token.cancel();
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

#[cfg(test)]
mod tests;
