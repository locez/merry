//! Runtime session orchestration for Merry.
//!
//! This crate owns the MVP runtime facade for session execution:
//!
//! - [`Runtime`] and [`RuntimeBuilder`] construct a session-scoped runtime.
//! - [`StepInput`], [`StepContext`], and [`RuntimeEventStream`] drive one
//!   provider-neutral runtime step at a time.
//! - [`RegisteredTool`] and [`ToolExecutor`] define the runtime-owned tool
//!   executor path.
//! - [`Runtime::record_artifact`], [`Runtime::evidence_ref`], and
//!   [`Runtime::ledger_projection`] expose session-owned artifact evidence and
//!   ledger reads without leaking provider wire formats.
//!
//! A few lower-level primitives are currently public because the in-memory MVP
//! still needs explicit composition and tests around structured state:
//! [`ArtifactRegistry`], [`TaskLedger`], [`ContextCompiler`], context entries,
//! ledger updates, and artifact records. Treat these as unstable
//! implementation-facing surfaces unless a [`Runtime`] method provides the same
//! read or mutation path.
//!
//! Runtime state is structured: summaries are navigation, exact evidence lives
//! in artifacts, and lifecycle facts are recorded before observable events are
//! emitted. The crate boundary is intentionally provider-neutral. Provider
//! adapters normalize requests and events in `merry-llm`/provider crates rather
//! than storing provider response formats here.
//!
//! The public API and crate boundaries are still unstable while the runtime
//! builder, event protocol, Memory Activation, and artifact/ledger persistence
//! contracts settle.

mod action_audit;
mod action_policy;
mod agent_loop;
mod artifact;
mod context;
mod error;
mod event_stream;
mod judgment;
mod ledger;
mod memory;
mod model_config;
mod process;
mod process_runner;
mod process_tool;
mod runtime;
mod session;
mod step;
mod summary_draft_promotion;
mod tool;

pub use agent_loop::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopConfigError, AgentLoopError, AgentLoopResult,
    AgentLoopStatus, DEFAULT_AGENT_LOOP_CONTINUATION_INPUT,
};
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
pub use model_config::RuntimeModelRole;
pub use process::{
    AcceptedLocalWorkspaceProcessAdmission, LocalWorkspaceProcessSandboxProfile,
    MAX_PROCESS_ARG_BYTES, MAX_PROCESS_ARGV_ITEMS, MAX_PROCESS_CWD_BYTES,
    MAX_PROCESS_OUTPUT_LIMIT_BYTES, MAX_PROCESS_STDIN_TEXT_BYTES, ProcessActionError,
    ProcessActionIntent, ProcessEnvPolicy, ProcessExecutionEvidence, ProcessExitStatus,
    ProcessPermissionProfileId, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
    ProcessRunnerFuture, ProcessRunnerOutput, ProcessRunnerResult,
    is_low_risk_process_action_intent, is_read_only_shell_process_action_intent,
};
pub use process_runner::TokioProcessRunner;
pub use process_tool::{ProcessCommandToolError, process_command_tool};
pub use runtime::{Runtime, RuntimeBuilder};
pub use step::{StepContext, StepInput};
pub use tool::{
    ActionExecutionEvidence, ActionProposal, ActionProposalError, ActionProposalEvidence,
    RegisteredTool, ToolActionKind, ToolActionProposalFuture, ToolActionProposalResult,
    ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutionResult,
    ToolExecutor, ToolExecutorFuture, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
};
