use crate::{FileSessionStore, RuntimeError, session::SessionState};
use merry_core::SessionId;

/// A validated persisted session loaded once before runtime construction.
///
/// The runtime owns the complete session state. Surface adapters may load and
/// validate a session before transferring this handle into
/// [`crate::RuntimeBuilder`] without loading the session a second time.
pub struct LoadedSession {
    state: SessionState,
}

impl LoadedSession {
    /// Loads and validates one session from the supplied store.
    pub async fn load(
        store: &FileSessionStore,
        session_id: &SessionId,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            state: SessionState::load_from(store, session_id).await?,
        })
    }

    pub(crate) fn into_state(self) -> SessionState {
        self.state
    }
}
