//! Runtime session orchestration for Merry.
//!
//! This crate owns the MVP runtime facade for session execution:
//!
//! - [`Runtime`] and [`RuntimeBuilder`] construct a session-scoped runtime.
//! - [`StepInput`], [`StepContext`], and [`RuntimeJournalEventStream`] drive one
//!   provider-neutral runtime step at a time.
//! - [`Tool`] defines the typed application-tool path; [`RegisteredTool`] and
//!   [`ToolExecutor`] define the runtime-owned integration path.
//! - [`Runtime::read_tool_output`] and [`Runtime::read_tool_output_preview`]
//!   expose session-owned tool outputs without leaking provider wire formats.
//!
//! A few lower-level primitives are currently public because the in-memory MVP
//! still needs explicit composition and tests around structured state:
//! [`ContextCompiler`] and context entries.
//! Treat these as unstable implementation-facing surfaces unless a [`Runtime`]
//! method provides the same read or mutation path.
//!
//! Runtime state is structured and recorded before observable events are
//! emitted. The crate boundary is intentionally provider-neutral. Provider
//! adapters normalize requests and events in `merry-llm`/provider crates
//! rather than storing provider response formats here.

mod action_policy;
mod agent_loop;
mod bridge;
mod checkpoint;
mod compaction;
mod context;
mod durable_memory;
mod error;
mod events;
mod final_output;
mod interactive;
mod memory;
mod model_completion;
mod model_config;
mod permission;
mod plan;
mod process;
mod process_background_tool;
mod process_tool;
mod profile;
mod prompt;
mod runtime;
#[cfg(test)]
mod schema_contract;
mod session;
mod session_projection;
mod session_store;
mod skill;
mod step;
mod subagent;
mod text;
mod todo;
mod token_estimate;
mod tool;
mod tool_admission;
mod tool_api;
mod tool_catalog;
mod tool_input;
mod tool_input_validation;
mod trajectory;
mod user_input;
mod workspace_scope;

pub use agent_loop::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopConfigError, AgentLoopError, AgentLoopResult,
    AgentLoopStatus, AgentRun, AgentRunMessage, DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS,
    StructuredOutputRetryPolicy,
};
pub use checkpoint::{
    CheckpointError, CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest,
    CheckpointSequenceRange, CheckpointSourceKind,
};
pub use compaction::{
    CitationCompactionInput, CitationCompactionPolicy, CompactionError, CompactionOutcome,
    ResolvedCitationCompactionBudget, citation_compaction_tail_directive,
};
pub use context::{
    CheckpointDecision, CompactedCheckpoint, CompactedCheckpointSummary, CompiledContext,
    CompiledContextSection, ContextBudget, ContextCompiler, ContextEntry, ContextError,
    ContextSummary, DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS, ProjectRules, ResolvedContextWindow,
    SessionContextSnapshot, TaskAnchor, decide_checkpoint, resolve_context_window,
};
pub use durable_memory::{DurableMemory, DurableMemoryError, MemoryLayout, MemoryWriteKind};
pub use error::RuntimeError;
pub use events::{RuntimeEventStream, RuntimeJournalEventStream};
pub use final_output::{
    FINAL_OUTPUT_TOOL_NAME, FinalOutput, FinalOutputContract, FinalOutputContractError,
};
pub use interactive::{
    AgentLoopControl, AgentLoopInput, InteractiveAgentRun, InteractiveError, InteractiveInputItem,
    InteractiveInputSnapshot, InteractivePrimaryModel, InteractiveRunEventStream, InteractiveRunId,
    InteractiveRunMessage, InteractiveSettingsUpdate, InteractiveSubagentSettings, InterruptReason,
    QueuedInputId,
};
pub use merry_core::{
    ContextWindowSource, ImageSummary, ToolOutput, ToolOutputKind, ToolOutputPreview,
    ToolOutputStream, ToolSpec,
};
pub use model_config::RuntimeModelRole;
pub use permission::{
    ChannelPermissionAdmissionSource, HostFallbackReason, HostIntegration, PermissionActor,
    PermissionAdmissionContext, PermissionAdmissionDecision, PermissionAdmissionError,
    PermissionAdmissionFuture, PermissionAdmissionReview, PermissionAdmissionReviewSource,
    PermissionAdmissionSource, PermissionRequest, PermissionReviewMode, PermissionReviewRequest,
    PermissionReviewResponse, PermissionReviewResponseError, PermissionReviewRisk,
    PermissionReviewRules, PermissionUserAuthorization, PermissionedAction, RequestedCapability,
    RequestedPathCapability, RuntimeTrustLevel, parse_permission_request, request_permissions_tool,
};
pub use plan::PlanError;
pub use process::{
    AcceptedLocalWorkspaceProcessAdmission, BackgroundProcessControl, BackgroundProcessId,
    BackgroundProcessLookupError, BackgroundProcessOutput, BackgroundProcessState,
    BackgroundProcessStop, BackgroundProcessSummary, DEFAULT_PROCESS_WAIT_TIMEOUT_SECS,
    LocalWorkspaceProcessSandboxProfile, MAX_PROCESS_ARG_BYTES, MAX_PROCESS_ARGV_ITEMS,
    MAX_PROCESS_CWD_BYTES, MAX_PROCESS_OUTPUT_LIMIT_BYTES, MAX_PROCESS_STDIN_TEXT_BYTES,
    MAX_PROCESS_WAIT_TIMEOUT_SECS, PermissionedProcessRunnerFactory, ProcessActionError,
    ProcessActionIntent, ProcessEnvPolicy, ProcessExecutionEvidence, ProcessExitStatus,
    ProcessOutputChunk, ProcessOutputEnvelope, ProcessOutputSink, ProcessOutputStream,
    ProcessPermissionProfileId, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
    ProcessRunnerFuture, ProcessRunnerOutput, ProcessRunnerResult,
    StaticPermissionedProcessRunnerFactory, host_shell_chain_separator,
    host_shell_has_unix_utilities, host_shell_name, is_low_risk_process_action_intent,
    is_read_only_shell_process_action_intent, shell_command_for_argv,
};
pub use process::{
    PreparedProcessPermission, ProcessPathGrant, ProcessPathGrantConstraint,
    ProcessSessionPermissionSnapshot, ProcessSessionPermissionView, ProcessSessionPermissions,
    SessionPermissionedProcessRunnerFactory,
};
pub use process_background_tool::{read_background_process_tool, stop_background_process_tool};
pub use process_tool::{ProcessCommandToolError, process_command_tool};
pub use profile::{
    AcceptedLocalWorkspaceProcessRunnerProfile, PathAccess, PathAccessRule, PathAccessRuleSource,
    RuntimeCapabilities, RuntimeProfile, RuntimeProfileBuilder, RuntimeProfileError,
};
pub use prompt::{PromptBlock, PromptError, PromptProfile, subagent_base_instructions};
pub use runtime::{
    CompactionConfig, HistoryRetention, Runtime, RuntimeBuilder, SegmentedArchiveConfig,
};
pub use session_projection::SessionTranscriptItem;
pub use session_store::{FileSessionStore, SessionReservation, SessionStoreError};
pub use skill::{
    FrontmatterError, SkillCatalog, SkillError, SkillLoadWarning, SkillLoadWarningReason,
    SkillMetadata,
};
pub use step::StepContext;
pub use subagent::{
    BackgroundHandoffNotice, BuiltChildRuntime, CancelSubagentsInput, ChildRuntimeFactory,
    ChildRuntimeInput, ChildWorkspaceFinalization, ChildWorkspaceLifecycle, ChildWorkspaceScope,
    ContextFileRef, DEFAULT_FOREGROUND_WAIT_BUDGET_SECS, DEFAULT_MAX_MODEL_TURNS,
    DEFAULT_MIN_MODEL_TURNS, MAX_CONTEXT_FILE_REFS, MAX_CONTEXT_FILES_TOTAL_BYTES,
    ParentPrimaryModel, PreloadedContextFile, RejectedSubagentView, ResumableChildSession,
    SUBAGENT_HANDOFF_CONTEXT_ID, SendSubagentMessageInput, SendSubagentMessageOptions,
    SendSubagentMessageOutcome, SendSubagentMessageOutput, SpawnSubagentTaskInput,
    SpawnSubagentsInput, SpawnSubagentsOutput, SpawnWaitMode, SpawnedSubagentStatusLabel,
    SpawnedSubagentView, SubagentActivityHub, SubagentActivityReceiver, SubagentConfig,
    SubagentError, SubagentInspectSession, SubagentLimitBehavior, SubagentManager,
    SubagentMessageDelivery, SubagentResultView, SubagentStatusLabel, SubagentStatusView,
    SubagentTaskSpec, SubagentWorkspaceMode, SubagentWorktreeBase, SubagentWorktreeView, WaitMode,
    WaitSubagentsInput, WaitSubagentsOutput, preload_context_files, render_subagent_handoff,
    subagent_registered_tools, subagent_registered_tools_with_roles, subagent_tool_specs,
    validate_no_write_scope_conflicts,
};
pub use tool::{
    ActionExecutionEvidence, ActionProposal, ActionProposalError, ActionProposalEvidence,
    RegisteredTool, ToolActionKind, ToolActionPreflight, ToolActionProposalFuture,
    ToolActionProposalResult, ToolConcurrency, ToolExecutionContext, ToolExecutionError,
    ToolExecutionOutcome, ToolExecutionResult, ToolExecutor, ToolExecutorFuture, ToolRunner,
    WorkspacePatchChangeEvidence, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
};
pub use tool_admission::ToolAdmission;
pub use tool_api::{Tool, ToolBuildError};
pub use tool_catalog::LoadedSession;
pub use user_input::{
    MAX_USER_IMAGE_DIMENSION, MAX_USER_IMAGE_PIXELS, MAX_USER_IMAGE_PNG_BYTES,
    MAX_USER_IMAGE_TOTAL_PNG_BYTES, MAX_USER_IMAGES, StepInput, UserImageInput, UserMessageInput,
    user_image_label,
};
