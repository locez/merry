use crate::{FileSessionStore, RuntimeError, session::SessionState};
use merry_core::{SessionId, SessionToolCatalog};

/// A validated persisted session loaded once before adapter-specific rebinding.
///
/// The runtime owns the complete session state. Surface adapters may inspect the
/// frozen external catalog before building tools, then transfer this handle into
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

    /// Returns the frozen external definitions, independently of availability.
    #[must_use]
    pub fn external_tool_catalog(&self) -> &SessionToolCatalog {
        self.state.external_tool_catalog()
    }

    pub(crate) fn into_state(self) -> SessionState {
        self.state
    }
}
