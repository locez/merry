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
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_stream::wrappers::ReceiverStream;

/// Stream of SDK-facing runtime events.
///
/// Dropping this stream aborts its projection task. The projection task owns the
/// underlying journal stream, so aborting it drops the journal stream and
/// preserves existing runtime-step cancellation behavior.
pub struct RuntimeEventStream {
    inner: Option<ReceiverStream<RuntimeEvent>>,
    producer_handle: Option<JoinHandle<()>>,
}

impl RuntimeEventStream {
    pub(crate) fn new(
        journal_stream: RuntimeJournalEventStream,
        runtime: Runtime,
        buffer_size: usize,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(buffer_size);
        let producer_handle = tokio::spawn(async move {
            project_journal_stream(journal_stream, runtime, sender).await;
        });

        Self {
            inner: Some(ReceiverStream::new(receiver)),
            producer_handle: Some(producer_handle),
        }
    }
}

impl Stream for RuntimeEventStream {
    type Item = RuntimeEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(None);
        };

        match Pin::new(inner).poll_next(cx) {
            Poll::Ready(None) => {
                self.producer_handle.take();
                Poll::Ready(None)
            }
            poll => poll,
        }
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
) {
    let mut projector = RuntimeEventProjector::new();

    while let Some(journal_event) = journal_stream.next().await {
        let public_event = projector.project(journal_event, &runtime).await;

        let Ok(Some(public_event)) = public_event else {
            continue;
        };

        if sender.send(public_event).await.is_err() {
            break;
        }
    }
}
