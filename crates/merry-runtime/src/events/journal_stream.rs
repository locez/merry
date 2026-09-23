//! Runtime journal event stream wrapper.
//!
//! The stream owns the lifetime of one active runtime step. Polling yields
//! provider-neutral [`RuntimeJournalEvent`] values after session state has been
//! recorded. The runtime updates read-model observers before enqueueing each
//! batch, so consuming this stream is not required to keep projections current.
//! Dropping the stream cancels and aborts the producer; the active step permit
//! is released when that producer future stops and drops its state.

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
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;

/// One atomic enqueue item in the internal semantic journal channel.
pub(crate) struct RuntimeJournalEventBatch(RuntimeJournalEventBatchKind);

pub(crate) type RuntimeRateUpdateSender = watch::Sender<Option<RuntimeJournalEvent>>;

pub(crate) fn runtime_rate_update_channel() -> (
    RuntimeRateUpdateSender,
    watch::Receiver<Option<RuntimeJournalEvent>>,
) {
    watch::channel(None)
}

// The single-event path is hot; boxing it would add an allocation to every journal emission.
#[allow(clippy::large_enum_variant)]
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

    /// Visits events without consuming the batch before it enters the channel.
    pub(crate) fn for_each(&self, mut visit: impl FnMut(&RuntimeJournalEvent)) {
        match &self.0 {
            RuntimeJournalEventBatchKind::Single(event) => visit(event),
            RuntimeJournalEventBatchKind::Multiple(events) => {
                for event in events {
                    visit(event);
                }
            }
        }
    }

    fn first_sequence(&self) -> u64 {
        match &self.0 {
            RuntimeJournalEventBatchKind::Single(event) => event.sequence,
            RuntimeJournalEventBatchKind::Multiple(events) => {
                events.first().map_or(0, |event| event.sequence)
            }
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

#[allow(clippy::large_enum_variant)]
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
/// discarding staged state or installing a durable commit. Semantic events are
/// delivered in sequence order; replaceable output-rate observations use a
/// latest-only channel and intermediate samples may be skipped.
pub struct RuntimeJournalEventStream {
    inner: Option<ReceiverStream<RuntimeJournalEventBatch>>,
    pending: Option<RuntimeJournalEventBatchIter>,
    pending_sequence: Option<u64>,
    pending_batch_started: bool,
    rate_updates: WatchStream<Option<RuntimeJournalEvent>>,
    pending_rate_update: Option<RuntimeJournalEvent>,
    rate_updates_closed: bool,
    cancellation_token: CancellationToken,
    producer_handle: Option<JoinHandle<()>>,
}

impl RuntimeJournalEventStream {
    pub(crate) fn new(
        inner: ReceiverStream<RuntimeJournalEventBatch>,
        rate_updates: watch::Receiver<Option<RuntimeJournalEvent>>,
        cancellation_token: CancellationToken,
        producer_handle: JoinHandle<()>,
    ) -> Self {
        Self {
            inner: Some(inner),
            pending: None,
            pending_sequence: None,
            pending_batch_started: false,
            rate_updates: WatchStream::from_changes(rate_updates),
            pending_rate_update: None,
            rate_updates_closed: false,
            cancellation_token,
            producer_handle: Some(producer_handle),
        }
    }
}

impl Stream for RuntimeJournalEventStream {
    type Item = RuntimeJournalEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if !self.rate_updates_closed {
                match Pin::new(&mut self.rate_updates).poll_next(cx) {
                    Poll::Ready(Some(Some(event))) => self.pending_rate_update = Some(event),
                    Poll::Ready(Some(None)) => {}
                    Poll::Ready(None) => self.rate_updates_closed = true,
                    Poll::Pending => {}
                }
            }

            let rate_precedes_pending_batch = !self.pending_batch_started
                && self.pending_rate_update.as_ref().is_some_and(|rate| {
                    self.pending_sequence
                        .is_some_and(|sequence| rate.sequence < sequence)
                });
            if let Some(pending) = self.pending.as_mut() {
                if rate_precedes_pending_batch {
                    return Poll::Ready(self.pending_rate_update.take());
                }
                let event = pending.next();
                if event.is_some() {
                    self.pending_batch_started = true;
                    return Poll::Ready(event);
                }
                self.pending = None;
                self.pending_sequence = None;
                self.pending_batch_started = false;
            }

            if let Some(inner) = self.inner.as_mut() {
                match Pin::new(inner).poll_next(cx) {
                    Poll::Ready(Some(batch)) => {
                        self.pending_sequence = Some(batch.first_sequence());
                        self.pending = Some(batch.into_iter());
                        self.pending_batch_started = false;
                        continue;
                    }
                    Poll::Ready(None) => {
                        self.inner = None;
                    }
                    Poll::Pending => {
                        if let Some(rate) = self.pending_rate_update.take() {
                            return Poll::Ready(Some(rate));
                        }
                    }
                }
            }

            if let Some(rate) = self.pending_rate_update.take() {
                return Poll::Ready(Some(rate));
            }

            if self.inner.is_none() && self.rate_updates_closed {
                self.producer_handle.take();
                return Poll::Ready(None);
            }
            return Poll::Pending;
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
    use futures_util::StreamExt;
    use merry_core::{RuntimeJournalPayload, SessionId};
    use tokio::sync::mpsc;

    fn event(sequence: u64) -> RuntimeJournalEvent {
        RuntimeJournalEvent::new(
            SessionId::new("journal-event-batch-test").expect("valid session id"),
            sequence,
            RuntimeJournalPayload::StepStarted,
        )
    }

    fn rate_event(sequence: u64) -> RuntimeJournalEvent {
        RuntimeJournalEvent::new(
            SessionId::new("journal-event-batch-test").expect("valid session id"),
            sequence,
            RuntimeJournalPayload::ModelOutputRateUpdated { rate: None },
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

    #[tokio::test]
    async fn rate_updates_replace_unread_values_instead_of_accumulating() {
        let (rate_sender, rate_receiver) = runtime_rate_update_channel();
        let (event_sender, event_receiver) = mpsc::channel(1);
        event_sender
            .send(
                RuntimeJournalEventBatch::from_events(vec![event(1)])
                    .expect("non-empty event batch"),
            )
            .await
            .expect("event stream should be open");
        rate_sender.send_replace(Some(rate_event(2)));
        rate_sender.send_replace(Some(rate_event(3)));
        drop(rate_sender);
        drop(event_sender);

        let producer_handle = tokio::spawn(async {});
        let mut stream = RuntimeJournalEventStream::new(
            ReceiverStream::new(event_receiver),
            rate_receiver,
            CancellationToken::new(),
            producer_handle,
        );
        let events: Vec<_> = stream.by_ref().collect().await;

        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert!(matches!(
            events[1].payload,
            RuntimeJournalPayload::ModelOutputRateUpdated { rate: None }
        ));
    }

    #[tokio::test]
    async fn a_new_rate_replaces_a_pending_rate_before_rendering_it() {
        let (rate_sender, rate_receiver) = runtime_rate_update_channel();
        let (event_sender, event_receiver) = mpsc::channel(1);
        event_sender
            .send(
                RuntimeJournalEventBatch::from_events(vec![event(1), event(2)])
                    .expect("non-empty event batch"),
            )
            .await
            .expect("event stream should be open");
        drop(event_sender);
        rate_sender.send_replace(Some(rate_event(3)));

        let producer_handle = tokio::spawn(async {});
        let mut stream = RuntimeJournalEventStream::new(
            ReceiverStream::new(event_receiver),
            rate_receiver,
            CancellationToken::new(),
            producer_handle,
        );

        assert_eq!(
            stream.next().await.expect("first semantic event").sequence,
            1
        );
        rate_sender.send_replace(Some(rate_event(4)));
        assert_eq!(
            stream.next().await.expect("second semantic event").sequence,
            2
        );
        assert_eq!(stream.next().await.expect("latest rate event").sequence, 4);
        drop(rate_sender);
        assert!(stream.next().await.is_none());
    }
}
