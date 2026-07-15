//! Runtime session orchestration for Merry.
//!
//! This crate owns the MVP runtime facade for session execution:
//!
//! - [`Runtime`] and [`RuntimeBuilder`] construct a session-scoped runtime.
//! - [`StepInput`], [`StepContext`], and [`RuntimeJournalEventStream`] drive one
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
mod checkpoint;
mod compaction;
mod context;
mod error;
mod events;
mod final_output;
mod interactive;
mod judgment;
mod ledger;
mod memory;
mod model_config;
mod permission;
mod plan;
mod process;
mod process_runner;
mod process_tool;
mod profile;
mod runtime;
mod session;
mod session_projection;
mod session_store;
mod skill;
mod step;
mod subagent;
mod summary_draft_promotion;
mod token_estimate;
mod tool;
mod tool_input_validation;
mod user_input;
mod workspace_scope;

pub use agent_loop::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopConfigError, AgentLoopError,
    AgentLoopEventStream, AgentLoopResult, AgentLoopStatus, AgentLoopStreamMessage,
    DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS, DEFAULT_CODING_AGENT_MAX_MODEL_TURNS,
};
pub use artifact::{
    ArtifactContent, ArtifactContentKind, ArtifactError, ArtifactRecord, ArtifactRegistry,
    ImageArtifactMetadata, TextEvidencePage,
};
pub use checkpoint::{
    CheckpointEntry, CheckpointEntryId, CheckpointError, CheckpointHandoff,
    CheckpointHandoffAction, CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest,
    CheckpointSection, CheckpointSections, CheckpointSequenceRange, CheckpointSourceKind,
    CheckpointValidationPolicy, CitationBackedCheckpoint, CompactedCheckpointCandidate,
};
pub use compaction::{
    CitationCompactionInput, CitationCompactionPolicy, CompactionError, CompactionOutcome,
    ResolvedCitationCompactionBudget, citation_compaction_response_schema,
    citation_compaction_system_prompt,
};
pub use context::{
    CheckpointDecision, CompactedCheckpoint, CompactedCheckpointSummary, CompiledContext,
    CompiledContextSection, ContextBudget, ContextBudgetPolicy, ContextCompiler, ContextEntry,
    ContextError, ContextEvidence, ContextSummary, DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS,
    ProjectRules, ResolvedContextWindow, SessionContextSnapshot, TaskAnchor, decide_checkpoint,
    resolve_context_window,
};
pub use error::RuntimeError;
pub use events::{RuntimeEventStream, RuntimeJournalEventStream};
pub use final_output::{
    FINAL_OUTPUT_TOOL_NAME, FinalOutput, FinalOutputContract, FinalOutputContractError,
};
pub use interactive::{
    AgentLoopControl, AgentLoopInput, InteractiveAgentRun, InteractiveError, InteractiveInputItem,
    InteractiveInputSnapshot, InteractivePrimaryModel, InteractiveRunEventStream, InteractiveRunId,
    InteractiveSettingsUpdate, InteractiveSubagentSettings, InterruptReason,
};
pub use ledger::{
    CompactLedgerText, LedgerFactKind, LedgerProjection, LedgerProjectionSnapshot, LedgerScope,
    LedgerUpdate, LedgerUpdateKind, LedgerValidationError, LifecycleFact, TaskLedger,
};
pub use merry_core::ContextWindowSource;
pub use model_config::RuntimeModelRole;
pub use permission::{
    PermissionAdmissionContext, PermissionAdmissionDecision, PermissionAdmissionError,
    PermissionAdmissionFuture, PermissionAdmissionReview, PermissionAdmissionSource,
    PermissionRequest, PermissionReviewMode, PermissionedAction, RequestedCapability,
    RequestedPathCapability, RuntimeTrustLevel, request_permissions_tool,
};
pub use plan::{
    BeginPlanInput, BeginPlanOutput, ControlPlanAttemptInput, PlanApprovalInput, PlanChangeInput,
    PlanControllerError, PlanDecompositionInput, PlanError, PlanExecutionIntent, PlanNodeInput,
    PlanNodeReferenceInput, PlanSubagentControl, PlanUpdateOutput, ReadPlanInput,
    ReportPlanAttemptInput, ReportPlanProgressInput, UpdatePlanInput,
};
pub use process::{
    AcceptedLocalWorkspaceProcessAdmission, LocalWorkspaceProcessSandboxProfile,
    MAX_PROCESS_ARG_BYTES, MAX_PROCESS_ARGV_ITEMS, MAX_PROCESS_CWD_BYTES,
    MAX_PROCESS_OUTPUT_LIMIT_BYTES, MAX_PROCESS_STDIN_TEXT_BYTES, PermissionedProcessRunnerFactory,
    ProcessActionError, ProcessActionIntent, ProcessEnvPolicy, ProcessExecutionEvidence,
    ProcessExitStatus, ProcessPermissionProfileId, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput, ProcessRunnerResult,
    StaticPermissionedProcessRunnerFactory, is_low_risk_process_action_intent,
    is_read_only_shell_process_action_intent,
};
pub use process_runner::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, TokioProcessRunner,
};
pub use process_tool::{ProcessCommandToolError, process_command_tool};
pub use profile::{
    AcceptedLocalWorkspaceProcessRunnerProfile, PathAccess, PathAccessRule, PathAccessRuleSource,
    RuntimeCapabilities, RuntimeProfile, RuntimeProfileBuilder, RuntimeProfileError,
};
pub use runtime::{AutomaticCompactionConfig, Runtime, RuntimeBuilder};
pub use session_projection::SessionTranscriptItem;
pub use session_store::{FileSessionStore, SessionStoreError};
pub use skill::{SkillCatalog, SkillError, SkillLoadWarning, SkillMetadata};
pub use step::StepContext;
pub use subagent::{
    CancelSubagentsInput, ChildRuntimeFactory, ChildRuntimeInput, ChildWorkspaceScope,
    DEFAULT_MAX_MODEL_TURNS, PlanLinkRuntime, PlanSubagentScope, RejectedSubagentView,
    SpawnSubagentTaskInput, SpawnSubagentsInput, SpawnSubagentsOutput, SpawnedSubagentStatusLabel,
    SpawnedSubagentView, SubagentActivityHub, SubagentActivityReceiver, SubagentConfig,
    SubagentError, SubagentManager, SubagentResultView, SubagentStatusLabel, SubagentStatusView,
    SubagentTaskSpec, WaitMode, WaitSubagentsInput, WaitSubagentsOutput, subagent_registered_tools,
    subagent_tool_specs, validate_no_write_scope_conflicts,
};
pub use tool::{
    ActionExecutionEvidence, ActionProposal, ActionProposalError, ActionProposalEvidence,
    RegisteredTool, ToolActionKind, ToolActionPreflight, ToolActionProposalFuture,
    ToolActionProposalResult, ToolConcurrency, ToolExecutionContext, ToolExecutionError,
    ToolExecutionOutcome, ToolExecutionResult, ToolExecutor, ToolExecutorFuture, ToolRunner,
    WorkspacePatchChangeEvidence, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
};
pub use user_input::{
    MAX_USER_IMAGE_DIMENSION, MAX_USER_IMAGE_PIXELS, MAX_USER_IMAGE_PNG_BYTES,
    MAX_USER_IMAGE_TOTAL_PNG_BYTES, MAX_USER_IMAGES, StepInput, UserImageInput, UserMessageInput,
    user_image_label,
};
