//! Deterministic context compiler and provider-neutral context state.
//!
//! This module is the stable facade for context contracts. Implementation
//! responsibilities live in focused child modules: budget calculation, owned
//! context input models, and deterministic provider-visible projection.
//!
//! [`SessionContextSnapshot`] is intentionally opaque and created by the
//! runtime session that owns both context entries and artifacts. The compiler
//! accepts snapshots rather than arbitrary caller-paired entries and registries
//! so evidence validation is tied to the owning session.

use crate::{artifact::ArtifactError, checkpoint::CheckpointError};
use merry_core::ArtifactId;
use thiserror::Error;

#[path = "context/budget.rs"]
mod budget;
#[path = "context/evidence.rs"]
mod evidence;
#[path = "context/input.rs"]
mod input;
#[path = "context/projection.rs"]
mod projection;

pub use budget::{
    CheckpointDecision, ContextBudget, ContextBudgetPolicy, ResolvedContextWindow,
    decide_checkpoint, resolve_context_window,
};
pub use input::{
    CompactedCheckpoint, CompactedCheckpointSummary, ContextEntry, ContextEvidence, ContextSummary,
    ProjectRules, SessionContextSnapshot, TaskAnchor,
};
pub(crate) use input::{PersistedCompactedCheckpoint, stable_content_hash};
pub(crate) use projection::compacted_checkpoint_wrapper_token_ceiling;
pub use projection::{CompiledContext, CompiledContextSection, ContextCompiler};

/// Coding-agent fallback used when config, provider metadata, and model catalogs are absent.
pub const DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS: u64 = 272_000;
/// Errors raised while constructing or compiling structured context.
///
/// These errors protect context invariants and the compile-time evidence
/// contract: public summary text and crate-internal memory projections can
/// enter compiled context only after their exact evidence resolves to readable
/// artifacts.
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

    /// More than one persisted summary matched the construction-owned seed fingerprint.
    #[error("construction context seed {id} has multiple managed predecessors")]
    AmbiguousConstructionContextSeed {
        /// Construction context summary identifier with ambiguous ownership.
        id: String,
    },

    /// A deterministic construction seed artifact id was occupied by different content.
    #[error("construction context seed {id} conflicts with artifact {artifact_id}")]
    ConstructionContextSeedArtifactConflict {
        /// Construction context summary identifier being reconciled.
        id: String,
        /// Occupied deterministic artifact identifier.
        artifact_id: ArtifactId,
    },

    /// Summary text was provided without exact evidence metadata.
    #[error("context summary {id} has no exact evidence references")]
    SummaryWithoutEvidence {
        /// Summary identifier that failed evidence validation.
        id: String,
    },

    /// Summary evidence did not resolve to readable artifact content.
    #[error("context summary {summary_id} references unreadable evidence {artifact_id}: {source}")]
    UnreadableEvidence {
        /// Summary identifier that linked the unreadable evidence.
        summary_id: String,
        /// Evidence artifact identifier.
        artifact_id: ArtifactId,
        /// Artifact registry read error.
        #[source]
        source: ArtifactError,
    },

    /// Internal memory text was provided without exact evidence metadata.
    #[error("memory item {memory_id} has no exact evidence references")]
    MemoryWithoutEvidence {
        /// Memory identifier that failed evidence validation.
        memory_id: String,
    },

    /// Internal memory evidence did not resolve to readable artifact content.
    #[error("memory item {memory_id} references unreadable evidence {artifact_id}: {source}")]
    UnreadableMemoryEvidence {
        /// Memory identifier that linked the unreadable evidence.
        memory_id: String,
        /// Evidence artifact identifier.
        artifact_id: ArtifactId,
        /// Artifact registry read error.
        #[source]
        source: ArtifactError,
    },

    /// Citation-backed checkpoint state failed validation.
    #[error("checkpoint state error: {source}")]
    Checkpoint {
        /// Checkpoint validation source error.
        #[from]
        source: CheckpointError,
    },
}

#[cfg(test)]
mod tests;
