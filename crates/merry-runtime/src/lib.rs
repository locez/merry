//! Runtime session, ledger, artifact, and checkpoint orchestration for Merry.

mod artifact;
mod context;
mod error;
mod event_stream;
mod ledger;
mod runtime;
mod session;
mod step;

pub use artifact::{
    ArtifactContent, ArtifactContentKind, ArtifactError, ArtifactRecord, ArtifactRegistry,
};
pub use context::{
    CompiledContext, CompiledContextSection, ContextCompiler, ContextEntry, ContextError,
    ContextEvidence, ContextSummary, SessionContextSnapshot,
};
pub use error::RuntimeError;
pub use event_stream::RuntimeEventStream;
pub use ledger::{
    CompactLedgerText, LedgerFactKind, LedgerProjection, LedgerProjectionSnapshot, LedgerScope,
    LedgerUpdate, LedgerUpdateKind, LedgerValidationError, LifecycleFact, TaskLedger,
};
pub use runtime::{Runtime, RuntimeBuilder};
pub use step::{StepContext, StepInput};
