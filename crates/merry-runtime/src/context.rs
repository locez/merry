//! Deterministic context compiler and provider-neutral context state.
//!
//! This module is the stable facade for context contracts. Implementation
//! responsibilities live in focused child modules: budget calculation, owned
//! context input models, and deterministic provider-visible projection.

use thiserror::Error;

#[path = "context/budget.rs"]
mod budget;
#[path = "context/input.rs"]
mod input;
#[path = "context/projection.rs"]
mod projection;

pub use budget::{
    CheckpointDecision, ContextBudget, ResolvedContextWindow, decide_checkpoint,
    resolve_context_window,
};
pub(crate) use input::stable_content_hash;
pub use input::{
    CompactedCheckpoint, CompactedCheckpointSummary, ContextEntry, ContextSummary, ProjectRules,
    SessionContextSnapshot, TaskAnchor,
};
pub(crate) use projection::compacted_checkpoint_wrapper_token_ceiling;
pub use projection::{CompiledContext, CompiledContextSection, ContextCompiler};

/// Coding-agent fallback used when config, provider metadata, and model catalogs are absent.
pub const DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS: u64 = 272_000;

/// Errors raised while constructing or compiling structured context.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextError {
    /// A required context field was blank.
    #[error("{field} must not be blank")]
    BlankField {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// A context field contained unsupported control characters.
    #[error("{field} must not contain control characters other than newline or tab")]
    InvalidControlCharacter {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// Context budget inputs could not produce a valid body budget.
    #[error("invalid context budget: {reason}")]
    InvalidBudget {
        /// Actionable reason the budget was rejected.
        reason: &'static str,
    },

    /// Context window metadata could not produce a valid window.
    #[error("invalid context window: {reason}")]
    InvalidContextWindow {
        /// Actionable reason the window was rejected.
        reason: &'static str,
    },

    /// Internal memory text was provided without provenance metadata.
    #[error("memory item {memory_id} has no provenance metadata")]
    MemoryWithoutEvidence {
        /// Memory identifier that failed provenance validation.
        memory_id: String,
    },
}

#[cfg(test)]
mod tests;
