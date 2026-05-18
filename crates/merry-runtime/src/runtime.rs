//! Runtime builder and step execution skeleton.

use crate::{
    ArtifactContent, ContextEntry, ContextSummary, RuntimeError, RuntimeEventStream,
    SessionContextSnapshot,
    event_stream::ActiveStepPermit,
    session::SessionState,
    step::{StepContext, StepInput},
};
use merry_core::{
    ArtifactId, ArtifactRef, ErrorInfo, EvidenceLocator, EvidenceRef, RuntimeEvent, SessionId,
};
use std::{
    num::NonZeroUsize,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::{Mutex, mpsc, mpsc::Permit};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

const DEFAULT_EVENT_BUFFER_SIZE: usize = 16;

/// Merry runtime handle for one session.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl Runtime {
    /// Creates a runtime builder for the provided session.
    #[must_use]
    pub fn builder(session_id: SessionId) -> RuntimeBuilder {
        RuntimeBuilder::new(session_id)
    }

    /// Starts a runtime step and returns its event stream.
    pub fn step(
        &self,
        _input: StepInput,
        context: StepContext,
    ) -> Result<RuntimeEventStream, RuntimeError> {
        let active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        let parent_token = context.into_cancellation_token();
        let step_token = parent_token.child_token();
        let producer_token = step_token.clone();
        let (sender, receiver) = mpsc::channel(self.inner.event_buffer_size.get());
        let inner = Arc::clone(&self.inner);

        let producer_handle = tokio::spawn(async move {
            run_step(inner, sender, producer_token, active_permit).await;
        });

        Ok(RuntimeEventStream::new(
            ReceiverStream::new(receiver),
            step_token,
            producer_handle,
        ))
    }

    /// Records exact artifact state into the owning session.
    pub async fn record_artifact(
        &self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<ArtifactRef, RuntimeError> {
        let mut session = self.inner.session.lock().await;
        session
            .record_artifact(artifact, content)
            .map_err(Into::into)
    }

    /// Creates an exact evidence reference from artifact state owned by this session.
    pub async fn evidence_ref(
        &self,
        artifact_id: &ArtifactId,
        locator: EvidenceLocator,
    ) -> Result<EvidenceRef, RuntimeError> {
        let session = self.inner.session.lock().await;
        session
            .evidence_ref(artifact_id, locator)
            .map_err(Into::into)
    }

    /// Records a structured context entry into the owning session.
    pub async fn record_context_entry(&self, entry: ContextEntry) {
        let mut session = self.inner.session.lock().await;
        session.record_context_entry(entry);
    }

    /// Records a summary context entry into the owning session.
    pub async fn record_context_summary(&self, summary: ContextSummary) {
        self.record_context_entry(ContextEntry::summary(summary))
            .await
    }

    /// Builds a sealed context snapshot from session-owned context and artifacts.
    pub async fn context_snapshot(&self) -> SessionContextSnapshot {
        let session = self.inner.session.lock().await;
        session.context_snapshot()
    }
}

/// Builder for a Merry runtime.
pub struct RuntimeBuilder {
    session_id: SessionId,
    event_buffer_size: NonZeroUsize,
}

impl RuntimeBuilder {
    fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            event_buffer_size: NonZeroUsize::new(DEFAULT_EVENT_BUFFER_SIZE)
                .expect("default event buffer size is non-zero"),
        }
    }

    /// Sets the bounded event channel buffer size.
    #[must_use]
    pub fn event_buffer_size(mut self, event_buffer_size: NonZeroUsize) -> Self {
        self.event_buffer_size = event_buffer_size;
        self
    }

    /// Builds the runtime.
    pub fn build(self) -> Result<Runtime, RuntimeError> {
        Ok(Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: self.session_id.clone(),
                session: Mutex::new(SessionState::new(self.session_id)),
                active_step: Arc::new(AtomicBool::new(false)),
                event_buffer_size: self.event_buffer_size,
            }),
        })
    }
}

struct RuntimeInner {
    session_id: SessionId,
    session: Mutex<SessionState>,
    active_step: Arc<AtomicBool>,
    event_buffer_size: NonZeroUsize,
}

async fn run_step(
    inner: Arc<RuntimeInner>,
    sender: mpsc::Sender<RuntimeEvent>,
    token: CancellationToken,
    _active_permit: ActiveStepPermit,
) {
    if token.is_cancelled() {
        let _ = send_cancelled_event(&inner, &sender).await;
        return;
    }

    if !send_normal_event(&inner, &sender, &token, |session| {
        session.record_session_started_if_needed()
    })
    .await
    {
        return;
    }

    if token.is_cancelled() {
        let _ = send_cancelled_event(&inner, &sender).await;
        return;
    }

    if !send_normal_event(&inner, &sender, &token, |session| {
        Some(session.record_step_started())
    })
    .await
    {
        return;
    }

    if token.is_cancelled() {
        let _ = send_cancelled_event(&inner, &sender).await;
        return;
    }

    let _ = send_normal_event(&inner, &sender, &token, |session| {
        Some(session.record_step_completed())
    })
    .await;
}

async fn send_normal_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    make_event: impl FnOnce(&mut SessionState) -> Option<RuntimeEvent>,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        make_event(&mut session)
    };

    if let Some(event) = event {
        permit.send(event);
    }

    true
}

async fn reserve_normal_event_slot<'a>(
    sender: &'a mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
) -> Option<Permit<'a, RuntimeEvent>> {
    if token.is_cancelled() || sender.is_closed() {
        return None;
    }

    tokio::select! {
        biased;
        () = token.cancelled() => None,
        () = sender.closed() => None,
        permit = sender.reserve() => permit.ok(),
    }
}

async fn send_cancelled_event(inner: &RuntimeInner, sender: &mpsc::Sender<RuntimeEvent>) -> bool {
    if sender.is_closed() {
        return false;
    }

    let Ok(permit) = sender.try_reserve() else {
        return false;
    };

    if sender.is_closed() {
        return false;
    }

    let diagnostic = ErrorInfo::new("cancelled", "runtime step cancelled")
        .expect("static cancellation diagnostic is valid");
    let event = {
        let mut session = inner.session.lock().await;
        session.record_cancelled(diagnostic)
    };
    permit.send(event);
    true
}

#[cfg(test)]
mod tests {
    use super::{RuntimeInner, send_cancelled_event};
    use crate::session::SessionState;
    use merry_core::{RuntimeEvent, RuntimeEventKind, SessionId};
    use std::{
        num::NonZeroUsize,
        sync::{Arc, atomic::AtomicBool},
    };
    use tokio::sync::{Mutex, mpsc};

    fn runtime_inner() -> RuntimeInner {
        let session_id = SessionId::new("runtime-send-test").expect("valid session id");
        RuntimeInner {
            session_id: session_id.clone(),
            session: Mutex::new(SessionState::new(session_id)),
            active_step: Arc::new(AtomicBool::new(false)),
            event_buffer_size: NonZeroUsize::new(1).expect("non-zero buffer"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_event_send_returns_false_when_channel_is_full() {
        let inner = runtime_inner();
        let (sender, _receiver) = mpsc::channel(1);
        sender
            .send(RuntimeEvent::new(
                inner.session_id.clone(),
                0,
                RuntimeEventKind::StepStarted,
            ))
            .await
            .expect("receiver remains open");

        let sent = send_cancelled_event(&inner, &sender).await;

        assert!(!sent);
    }
}
