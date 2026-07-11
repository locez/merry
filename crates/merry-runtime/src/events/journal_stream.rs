//! Runtime journal event stream wrapper.
//!
//! The stream owns the lifetime of one active runtime step. Polling yields
//! provider-neutral [`RuntimeJournalEvent`] values after session state has been
//! recorded. Dropping the stream cancels and aborts the producer; the active
//! step permit is released when that producer future stops and drops its state.

use futures_core::Stream;
use merry_core::RuntimeJournalEvent;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

/// One atomic enqueue item in the internal journal channel.
pub(crate) struct RuntimeJournalEventBatch(RuntimeJournalEventBatchKind);

enum RuntimeJournalEventBatchKind {
    Single(RuntimeJournalEvent),
    Multiple(Vec<RuntimeJournalEvent>),
}

impl RuntimeJournalEventBatch {
    pub(crate) fn pair(first: RuntimeJournalEvent, second: RuntimeJournalEvent) -> Self {
        Self(RuntimeJournalEventBatchKind::Multiple(vec![first, second]))
    }

    pub(crate) fn from_events(mut events: Vec<RuntimeJournalEvent>) -> Option<Self> {
        match events.len() {
            0 => None,
            1 => Some(Self(RuntimeJournalEventBatchKind::Single(
                events.pop().expect("one event remains"),
            ))),
            _ => Some(Self(RuntimeJournalEventBatchKind::Multiple(events))),
        }
    }

    fn into_iter(self) -> RuntimeJournalEventBatchIter {
        match self.0 {
            RuntimeJournalEventBatchKind::Single(event) => {
                RuntimeJournalEventBatchIter::Single(Some(event))
            }
            RuntimeJournalEventBatchKind::Multiple(events) => {
                RuntimeJournalEventBatchIter::Multiple(events.into_iter())
            }
        }
    }
}

impl From<RuntimeJournalEvent> for RuntimeJournalEventBatch {
    fn from(event: RuntimeJournalEvent) -> Self {
        Self(RuntimeJournalEventBatchKind::Single(event))
    }
}

enum RuntimeJournalEventBatchIter {
    Single(Option<RuntimeJournalEvent>),
    Multiple(std::vec::IntoIter<RuntimeJournalEvent>),
}

impl Iterator for RuntimeJournalEventBatchIter {
    type Item = RuntimeJournalEvent;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Single(event) => event.take(),
            Self::Multiple(events) => events.next(),
        }
    }
}

/// Stream of provider-neutral runtime journal events.
///
/// A stream is returned by [`crate::Runtime::step`] and
/// [`crate::Runtime::journal_stream`]. It should be driven until completion
/// when callers want the producer to finish normally. Dropping it is the
/// cancellation path for the active step. The permit may remain active after
/// the producer stops while an in-flight persistence transaction finishes
/// discarding staged state or installing a durable commit.
pub struct RuntimeJournalEventStream {
    inner: Option<ReceiverStream<RuntimeJournalEventBatch>>,
    pending: Option<RuntimeJournalEventBatchIter>,
    cancellation_token: CancellationToken,
    producer_handle: Option<JoinHandle<()>>,
}

impl RuntimeJournalEventStream {
    pub(crate) fn new(
        inner: ReceiverStream<RuntimeJournalEventBatch>,
        cancellation_token: CancellationToken,
        producer_handle: JoinHandle<()>,
    ) -> Self {
        Self {
            inner: Some(inner),
            pending: None,
            cancellation_token,
            producer_handle: Some(producer_handle),
        }
    }
}

impl Stream for RuntimeJournalEventStream {
    type Item = RuntimeJournalEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(event) = self.pending.as_mut().and_then(Iterator::next) {
                return Poll::Ready(Some(event));
            }
            self.pending = None;

            let Some(inner) = self.inner.as_mut() else {
                return Poll::Ready(None);
            };

            match Pin::new(inner).poll_next(cx) {
                Poll::Ready(Some(batch)) => {
                    self.pending = Some(batch.into_iter());
                }
                Poll::Ready(None) => {
                    self.producer_handle.take();
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Drop for RuntimeJournalEventStream {
    fn drop(&mut self) {
        self.inner.take();
        self.cancellation_token.cancel();

        if let Some(handle) = self.producer_handle.take() {
            handle.abort();
        }
    }
}

pub(crate) struct ActiveStepPermit {
    inner: Arc<ActiveStepPermitInner>,
}

impl ActiveStepPermit {
    pub(crate) fn acquire(active: Arc<AtomicBool>) -> Option<Self> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;

        Some(Self {
            inner: Arc::new(ActiveStepPermitInner { active }),
        })
    }
}

impl Clone for ActiveStepPermit {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Drop for ActiveStepPermitInner {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

struct ActiveStepPermitInner {
    active: Arc<AtomicBool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{RuntimeJournalPayload, SessionId};

    fn event(sequence: u64) -> RuntimeJournalEvent {
        RuntimeJournalEvent::new(
            SessionId::new("journal-event-batch-test").expect("valid session id"),
            sequence,
            RuntimeJournalPayload::StepStarted,
        )
    }

    #[test]
    fn event_batch_rejects_empty_and_preserves_all_events_in_order() {
        assert!(RuntimeJournalEventBatch::from_events(Vec::new()).is_none());

        let events = RuntimeJournalEventBatch::from_events(vec![event(4), event(5), event(6)])
            .expect("non-empty batch");
        assert_eq!(
            events
                .into_iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [4, 5, 6]
        );
    }
}
