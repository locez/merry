//! Public runtime event stream wrapper.

use super::{RuntimeEventProjector, RuntimeJournalEventStream};
use crate::Runtime;
use futures_core::Stream;
use futures_util::StreamExt;
use merry_core::RuntimeEvent;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::watch;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::wrappers::WatchStream;

/// Stream of SDK-facing runtime events.
///
/// Dropping this stream aborts its projection task. The projection task owns the
/// underlying journal stream, so aborting it drops the journal stream and
/// preserves existing runtime-step cancellation behavior. Semantic events are
/// buffered in order; output-rate events are latest-only telemetry and may be
/// coalesced while the consumer is busy.
pub struct RuntimeEventStream {
    inner: Option<ReceiverStream<RuntimeEvent>>,
    rate_updates: WatchStream<Option<RuntimeEvent>>,
    rate_updates_closed: bool,
    pending_rate_update: Option<RuntimeEvent>,
    producer_handle: Option<JoinHandle<()>>,
}

impl RuntimeEventStream {
    pub(crate) fn new(
        journal_stream: RuntimeJournalEventStream,
        runtime: Runtime,
        buffer_size: usize,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(buffer_size);
        let (rate_sender, rate_receiver) = watch::channel(None);
        let producer_handle = tokio::spawn(async move {
            project_journal_stream(journal_stream, runtime, sender, rate_sender).await;
        });

        Self {
            inner: Some(ReceiverStream::new(receiver)),
            rate_updates: WatchStream::from_changes(rate_receiver),
            rate_updates_closed: false,
            pending_rate_update: None,
            producer_handle: Some(producer_handle),
        }
    }
}

impl Stream for RuntimeEventStream {
    type Item = RuntimeEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if !self.rate_updates_closed {
            match Pin::new(&mut self.rate_updates).poll_next(cx) {
                Poll::Ready(Some(Some(event))) => self.pending_rate_update = Some(event),
                Poll::Ready(Some(None)) => {}
                Poll::Ready(None) => self.rate_updates_closed = true,
                Poll::Pending => {}
            }
        }

        if let Some(inner) = self.inner.as_mut() {
            match Pin::new(inner).poll_next(cx) {
                Poll::Ready(Some(event)) => return Poll::Ready(Some(event)),
                Poll::Ready(None) => self.inner = None,
                Poll::Pending => {
                    if let Some(event) = self.pending_rate_update.take() {
                        return Poll::Ready(Some(event));
                    }
                }
            }
        }

        if let Some(event) = self.pending_rate_update.take() {
            return Poll::Ready(Some(event));
        }
        if self.inner.is_none() && self.rate_updates_closed {
            self.producer_handle.take();
            return Poll::Ready(None);
        }
        Poll::Pending
    }
}

impl Drop for RuntimeEventStream {
    fn drop(&mut self) {
        self.inner.take();

        if let Some(handle) = self.producer_handle.take() {
            handle.abort();
        }
    }
}

async fn project_journal_stream(
    mut journal_stream: RuntimeJournalEventStream,
    runtime: Runtime,
    sender: mpsc::Sender<RuntimeEvent>,
    rate_sender: watch::Sender<Option<RuntimeEvent>>,
) {
    let mut projector = RuntimeEventProjector::new();

    while let Some(journal_event) = journal_stream.next().await {
        let public_event = projector.project(journal_event, &runtime).await;

        let Ok(Some(public_event)) = public_event else {
            continue;
        };

        if matches!(&public_event, RuntimeEvent::ModelOutputRateUpdated { .. }) {
            rate_sender.send_replace(Some(public_event));
        } else if sender.send(public_event).await.is_err() {
            break;
        }
    }
}
