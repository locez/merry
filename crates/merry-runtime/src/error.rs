//! Runtime error types.

use merry_core::{CoreError, SessionId};
use thiserror::Error;

/// Errors raised by Merry runtime construction and step admission.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// A new step was requested while another step still owns the runtime.
    #[error("runtime step already active for session {session_id}")]
    StepAlreadyActive {
        /// Session with an active step.
        session_id: SessionId,
    },

    /// Step input failed runtime validation.
    #[error("invalid step input: {reason}")]
    InvalidStepInput {
        /// Actionable validation detail.
        reason: &'static str,
    },

    /// A core protocol value could not be constructed.
    #[error("core protocol error while constructing runtime state: {source}")]
    Core {
        /// Source core validation error.
        #[from]
        source: CoreError,
    },
}
