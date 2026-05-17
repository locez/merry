//! Runtime session state and state-before-event helpers.

use crate::{
    artifact::ArtifactRegistry,
    ledger::{LedgerFactKind, TaskLedger},
};
use merry_core::{ErrorInfo, RuntimeEvent, RuntimeEventKind, SessionId};

/// Mutable runtime state for one session.
#[derive(Debug)]
pub(crate) struct SessionState {
    session_id: SessionId,
    next_sequence: u64,
    session_started: bool,
    ledger: TaskLedger,
    artifacts: ArtifactRegistry,
}

impl SessionState {
    pub(crate) fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            next_sequence: 0,
            session_started: false,
            ledger: TaskLedger::default(),
            artifacts: ArtifactRegistry::default(),
        }
    }

    pub(crate) fn record_session_started_if_needed(&mut self) -> Option<RuntimeEvent> {
        debug_assert!(self.artifacts.is_empty());
        if self.session_started {
            return None;
        }

        self.session_started = true;
        Some(self.record_event(
            RuntimeEventKind::SessionStarted,
            LedgerFactKind::SessionStarted,
        ))
    }

    pub(crate) fn record_step_started(&mut self) -> RuntimeEvent {
        debug_assert!(self.artifacts.is_empty());
        self.record_event(RuntimeEventKind::StepStarted, LedgerFactKind::StepStarted)
    }

    pub(crate) fn record_step_completed(&mut self) -> RuntimeEvent {
        debug_assert!(self.artifacts.is_empty());
        self.record_event(
            RuntimeEventKind::StepCompleted,
            LedgerFactKind::StepCompleted,
        )
    }

    pub(crate) fn record_cancelled(&mut self, diagnostic: ErrorInfo) -> RuntimeEvent {
        debug_assert!(self.artifacts.is_empty());
        self.record_event(
            RuntimeEventKind::Cancelled { diagnostic },
            LedgerFactKind::Cancelled,
        )
    }

    fn record_event(&mut self, kind: RuntimeEventKind, fact_kind: LedgerFactKind) -> RuntimeEvent {
        let sequence = self.next_sequence;
        self.ledger.record(sequence, fact_kind);
        self.next_sequence += 1;
        RuntimeEvent::new(self.session_id.clone(), sequence, kind)
    }
}

#[cfg(test)]
mod tests {
    use super::SessionState;
    use merry_core::{RuntimeEventKind, SessionId};

    fn session_id() -> SessionId {
        SessionId::new("session-state-test").expect("valid session id")
    }

    #[test]
    fn session_start_is_recorded_once_before_step_lifecycle() {
        let mut session = SessionState::new(session_id());

        let first = session
            .record_session_started_if_needed()
            .expect("first start should emit");
        let second = session.record_session_started_if_needed();
        let started = session.record_step_started();
        let completed = session.record_step_completed();

        assert!(matches!(first.kind, RuntimeEventKind::SessionStarted));
        assert!(second.is_none());
        assert_eq!(started.sequence, 1);
        assert_eq!(completed.sequence, 2);
    }
}
