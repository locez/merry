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

/// Stream of provider-neutral runtime journal events.
///
/// A stream is returned by [`crate::Runtime::step`] and
/// [`crate::Runtime::journal_stream`]. It should be driven until completion
/// when callers want the producer to finish normally. Dropping it is the
/// cancellation path for the active step, but the permit may remain active
/// briefly until the producer future is stopped.
pub struct RuntimeJournalEventStream {
    inner: Option<ReceiverStream<RuntimeJournalEvent>>,
    cancellation_token: CancellationToken,
    producer_handle: Option<JoinHandle<()>>,
}

impl RuntimeJournalEventStream {
    pub(crate) fn new(
        inner: ReceiverStream<RuntimeJournalEvent>,
        cancellation_token: CancellationToken,
        producer_handle: JoinHandle<()>,
    ) -> Self {
        Self {
            inner: Some(inner),
            cancellation_token,
            producer_handle: Some(producer_handle),
        }
    }
}

impl Stream for RuntimeJournalEventStream {
    type Item = RuntimeJournalEvent;

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
