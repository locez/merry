//! High-level coding session facade shared by terminal UI, headless CLI,
//! WebUI backends, and Rust SDK hosts.
//!
//! [`CodingSession`] owns the full coding-agent session lifecycle—including
//! exclusive OS-level session locking, resume error classification, durable
//! memory attachment, interactive and single-shot execution, and read-only
//! observability—without requiring callers to manipulate `merry-runtime`
//! primitives directly.

use crate::observability::TrajectoryHandle;
pub(crate) mod surface {
    pub use merry_coding::{
        CODING_AGENT_DYNAMIC_CONTEXT_LAYOUT, CODING_AGENT_POLICY_PROMPT, CODING_AGENT_PROFILE_ID,
        CODING_AGENT_STABLE_PREFIX_LAYOUT, CODING_LOOP_PROCESS_TOOL, CodingAgentProfile,
        CodingAgentProfileBuildError, CodingAgentProfileBuilder, CodingAgentProfileHash,
        CodingAgentRunPolicy, CodingAgentRunPolicyError, CodingAutoFallback,
        CodingFinalReportPolicy, CodingModelRoleConfig, CodingModelRoleConfigError,
        CodingPermissionMode, CodingPermissionPolicy, CodingPermissionPolicyError, CodingRuntime,
        CodingRuntimeBuildError, CodingRuntimeBuilder, CodingRuntimeInput, CodingSubagentsConfig,
        CodingWorkflowCommand, DEFAULT_CODING_AGENT_MAX_MODEL_TURNS,
        DEFAULT_CODING_SUBAGENT_MAX_MODEL_TURNS, MAX_ROLE_PROMPT_FILE_BYTES,
        MAX_ROOT_PROJECT_RULES_BYTES, MIN_CODING_SUBAGENT_MODEL_TURNS, ProjectRulesLoadError,
        ROOT_PROJECT_RULES_FILE, ResolvedSubagentRuntime, SubagentDefaultOverride,
        SubagentModelOverride, SubagentResolutionInput, SubagentRoleCatalog, SubagentRoleError,
        SubagentRoleOverlay, SubagentRoleSpec, SubagentWorkspaceAccess, WorkspaceToolLimits,
        WorktreeError, WorktreeGcReport, WorktreeIsolationManager, cleanup_orphaned_worktrees,
        cleanup_orphaned_worktrees_default, coding_agent, default_worktree_base_dir,
        load_root_project_rules, load_workspace_prompt_file, resolve_subagent_runtime,
    };
}
use merry_core::{
    PlanDocument, PlanSessionSnapshot, SessionId, SessionUsage, SubagentId, TodoItem, ToolCallId,
    ToolOutput,
};
use merry_llm::GenerationConfig;
pub use merry_runtime::{
    AgentLoopConfig, AgentLoopControl, AgentLoopInput, AgentLoopResult, AgentRun, AgentRunMessage,
    BackgroundProcessId, BackgroundProcessLookupError, BackgroundProcessOutput,
    BackgroundProcessStop, BackgroundProcessSummary, CitationCompactionPolicy,
    DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS, DEFAULT_FOREGROUND_WAIT_BUDGET_SECS, DurableMemory,
    FileSessionStore, InteractiveError, InteractiveRunEventStream, InteractiveRunMessage,
    InteractiveSettingsUpdate, InteractiveSubagentSettings, MemoryLayout, RuntimeError,
    RuntimeModelRole, SessionStoreError, SessionTranscriptItem, SkillMetadata,
    SubagentActivityReceiver, SubagentConfig, SubagentInspectSession, SubagentLimitBehavior,
    SubagentStatusView, WaitSubagentsOutput,
};
use merry_runtime::{LoadedSession, Runtime, SessionReservation, StepContext, StepInput};
use std::path::PathBuf;
pub use surface::*;
use thiserror::Error;

/// Whether [`CodingSessionBuilder::open`] starts a fresh session or resumes a
/// previously saved session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingSessionOpenMode {
    /// Start a new session and fail if persisted state already exists for this
    /// session ID.
    New,
    /// Resume an existing persisted session from the session store.
    Resume,
}

/// Structured classification for why a saved session cannot be resumed
/// directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingSessionResumeFailure {
    /// Another active process currently holds the exclusive session lock.
    AlreadyOpen,
    /// The saved session files are corrupt, unreadable, or from an incompatible
    /// schema version.
    IncompatibleFormat,
}

/// Errors raised while opening or resuming a persisted [`CodingSession`].
#[derive(Debug, Error)]
pub enum CodingSessionOpenError {
    /// A new session was requested for an ID that already has saved state.
    #[error("session {session_id} already has saved state")]
    SessionAlreadyExists {
        /// Conflicting session identifier.
        session_id: SessionId,
    },
    /// A saved session cannot be resumed directly (locked or incompatible).
    #[error("could not resume session {session_id}: {message}")]
    ResumeUnavailable {
        /// Why resuming failed.
        kind: CodingSessionResumeFailure,
        /// Session identifier that could not be resumed.
        session_id: SessionId,
        /// On-disk session directory path for diagnostics or recovery UI.
        session_dir: PathBuf,
        /// Underlying failure details.
        message: String,
    },
    /// The session store failed while reserving or inspecting session state.
    #[error("session store error: {source}")]
    SessionStore {
        /// Underlying session store failure.
        #[source]
        source: SessionStoreError,
    },
    /// Building the coding runtime failed.
    #[error(transparent)]
    Build {
        /// Underlying coding runtime build error.
        #[from]
        source: CodingRuntimeBuildError,
    },
}

/// Classifies whether a [`RuntimeError`] from loading a saved session maps to a
/// structured [`CodingSessionResumeFailure`].
#[must_use]
pub fn classify_resume_error(error: &RuntimeError) -> Option<CodingSessionResumeFailure> {
    match error {
        RuntimeError::SessionStore {
            source: SessionStoreError::SessionAlreadyReserved { .. },
        } => Some(CodingSessionResumeFailure::AlreadyOpen),
        RuntimeError::SessionStore {
            source:
                SessionStoreError::UnsupportedFormatVersion { .. }
                | SessionStoreError::InvalidDocument { .. }
                | SessionStoreError::Json { .. }
                | SessionStoreError::SessionIdMismatch { .. }
                | SessionStoreError::Io { .. },
        } => Some(CodingSessionResumeFailure::IncompatibleFormat),
        _ => None,
    }
}

/// Opens XDG durable memory for `input.root()`, attaching it when available or
/// logging a warning and continuing without durable memory on error.
#[must_use]
pub fn attach_durable_memory_from_env(input: CodingRuntimeInput) -> CodingRuntimeInput {
    let root = input.root().to_path_buf();
    match DurableMemory::open_from_env(&root) {
        Ok(memory) => input.durable_memory(memory),
        Err(error) => {
            tracing::warn!(
                event = "memory.open_failed",
                error = %error,
                "durable memory is unavailable; continuing without it"
            );
            input
        }
    }
}

/// Builder for a high-level [`CodingSession`].
///
/// Wraps [`CodingRuntimeBuilder`] with session locking, resume classification,
/// optional MCP toolset attachment, and environment-backed durable memory so
/// TUI, headless CLI, WebUI, and SDK hosts share one lifecycle contract.
pub struct CodingSessionBuilder {
    input: CodingRuntimeInput,
    permission_policy: CodingPermissionPolicy,
    session_store: Option<FileSessionStore>,
    loaded_session: Option<LoadedSession>,
    reservation: Option<SessionReservation>,
}

impl CodingSessionBuilder {
    /// Creates a new coding session builder from validated coding runtime inputs.
    #[must_use]
    pub fn new(input: CodingRuntimeInput) -> Self {
        Self {
            input,
            permission_policy: CodingPermissionPolicy::default(),
            session_store: None,
            loaded_session: None,
            reservation: None,
        }
    }

    /// Sets the coding permission review policy.
    #[must_use]
    pub fn permission_policy(mut self, policy: CodingPermissionPolicy) -> Self {
        self.permission_policy = policy;
        self
    }

    /// Attaches a session store for automatic savepoints when building directly
    /// with [`Self::build`].
    #[must_use]
    pub fn session_store(mut self, store: FileSessionStore) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Attaches discovered MCP meta-tools without exposing internal runtime
    /// tool registration types.
    #[cfg(feature = "mcp")]
    #[must_use]
    pub fn mcp_tools(mut self, toolset: crate::mcp::McpToolset) -> Self {
        let (tools, _warnings) = toolset.into_parts();
        self.input = self.input.with_extra_tools(tools);
        self
    }

    /// Opens XDG durable memory for the workspace root, logging a warning and
    /// continuing without durable memory if it is unavailable.
    #[must_use]
    pub fn with_durable_memory_from_env(mut self) -> Self {
        self.input = attach_durable_memory_from_env(self.input);
        self
    }

    /// Builds an in-memory or pre-configured [`CodingSession`] without
    /// acquiring a new reservation from a store.
    pub fn build(self) -> Result<CodingSession, CodingRuntimeBuildError> {
        let mut builder =
            CodingRuntimeBuilder::new(self.input).permission_policy(self.permission_policy);
        if let Some(loaded) = self.loaded_session {
            builder = builder.with_loaded_session(loaded);
        }
        if let Some(store) = self.session_store.clone() {
            builder = builder.session_store(store);
        }
        let coding_runtime = builder.build()?;
        let loop_config = coding_runtime.loop_config();
        let runtime = coding_runtime.into_runtime();
        Ok(CodingSession {
            handle: CodingSessionHandle { runtime },
            loop_config,
            store: self.session_store,
            reservation: self.reservation,
        })
    }

    /// Reserves the session lock in `store`, validates new-vs-resume state,
    /// loads any saved transcript when resuming, and builds a [`CodingSession`]
    /// that retains the exclusive lock for its lifetime.
    pub async fn open(
        mut self,
        store: FileSessionStore,
        mode: CodingSessionOpenMode,
    ) -> Result<CodingSession, CodingSessionOpenError> {
        let session_id = self.input.session_id().clone();
        let reservation = match store.reserve_session(&session_id).await {
            Ok(reservation) => reservation,
            Err(SessionStoreError::SessionAlreadyReserved { .. })
                if mode == CodingSessionOpenMode::Resume =>
            {
                return Err(CodingSessionOpenError::ResumeUnavailable {
                    kind: CodingSessionResumeFailure::AlreadyOpen,
                    session_dir: store.session_dir(&session_id),
                    session_id,
                    message: "session is already open in another process".to_owned(),
                });
            }
            Err(source) => return Err(CodingSessionOpenError::SessionStore { source }),
        };

        match mode {
            CodingSessionOpenMode::New => {
                let exists = store
                    .contains_session(&session_id)
                    .await
                    .map_err(|source| CodingSessionOpenError::SessionStore { source })?;
                if exists {
                    return Err(CodingSessionOpenError::SessionAlreadyExists { session_id });
                }
            }
            CodingSessionOpenMode::Resume => {
                let loaded = LoadedSession::load(&store, &session_id)
                    .await
                    .map_err(|error| CodingSessionOpenError::ResumeUnavailable {
                        kind: classify_resume_error(&error)
                            .unwrap_or(CodingSessionResumeFailure::IncompatibleFormat),
                        session_dir: store.session_dir(&session_id),
                        session_id: session_id.clone(),
                        message: error.to_string(),
                    })?;
                self.loaded_session = Some(loaded);
            }
        }

        self.session_store = Some(store);
        self.reservation = Some(reservation);
        self.build()
            .map_err(|source| CodingSessionOpenError::Build { source })
    }
}

/// Interactive handles returned by [`CodingSession::start_interactive`].
pub struct InteractiveCodingRun {
    /// Live stream of runtime events and turn messages.
    pub stream: InteractiveRunEventStream,
    /// Handle for submitting user prompts and queued inputs.
    pub input: AgentLoopInput,
    /// Handle for interrupting turns, compacting context, updating settings,
    /// and saving the session while running.
    pub control: AgentLoopControl,
    /// Watch channel carrying live UI-only subagent activity snapshots.
    pub subagent_activity: SubagentActivityReceiver,
    /// Loaded skill metadata available in this session.
    pub skills: Vec<SkillMetadata>,
}

/// High-level owned coding session with lifecycle lock management.
///
/// Hosts (TUI, headless CLI, WebUI, or Rust SDK applications) own one
/// `CodingSession` per active session and clone [`CodingSessionHandle`] into
/// background tasks, HTTP handlers, or UI effect loops.
pub struct CodingSession {
    handle: CodingSessionHandle,
    loop_config: AgentLoopConfig,
    store: Option<FileSessionStore>,
    reservation: Option<SessionReservation>,
}

impl CodingSession {
    /// Creates a [`CodingSessionBuilder`] for the supplied coding inputs.
    #[must_use]
    pub fn builder(input: CodingRuntimeInput) -> CodingSessionBuilder {
        CodingSessionBuilder::new(input)
    }

    /// Returns the session identifier.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        self.handle.session_id()
    }

    /// Returns a cheap, cloneable handle for session inspection, subagent
    /// control, background process control, and persistence.
    #[must_use]
    pub fn handle(&self) -> CodingSessionHandle {
        self.handle.clone()
    }

    /// Returns a read-only trajectory handle suitable for `merry-web` or other
    /// observability surfaces.
    #[must_use]
    pub fn trajectory_handle(&self) -> TrajectoryHandle {
        self.handle.trajectory_handle()
    }

    /// Returns the coding agent loop configuration for this session.
    #[must_use]
    pub fn loop_config(&self) -> AgentLoopConfig {
        self.loop_config.clone()
    }

    /// Starts an interactive coding run with live event streaming, input queue,
    /// runtime settings control, and subagent activity watch channel.
    pub async fn start_interactive(
        &self,
        generation: GenerationConfig,
    ) -> Result<InteractiveCodingRun, RuntimeError> {
        let skills = self.handle.skills().await;
        let interactive = self.handle.runtime.start_interactive_agent_run(
            StepContext::default().with_generation_config(generation),
            self.loop_config.clone(),
        )?;
        let subagent_activity = self.handle.subscribe_subagent_activity();
        let (stream, input, control) = interactive.split();
        Ok(InteractiveCodingRun {
            stream,
            input,
            control,
            subagent_activity,
            skills,
        })
    }

    /// Starts a single-shot streaming coding run (used by headless CLI or
    /// non-interactive Web/SDK tasks).
    pub fn run_stream(
        &self,
        task: &str,
        generation: GenerationConfig,
    ) -> Result<AgentRun, RuntimeError> {
        self.handle
            .run_agent_loop_stream(task, generation, self.loop_config.clone())
    }

    /// Releases any held session reservation and removes the session directory
    /// if the session exited without writing persisted files.
    pub async fn discard_if_empty(&mut self) {
        if let (Some(store), Some(reservation)) = (self.store.as_ref(), self.reservation.take()) {
            store
                .release_empty_reservation(self.session_id(), reservation)
                .await;
        }
    }

    /// Consumes this session and returns the underlying runtime handle for
    /// advanced low-level integrations.
    #[doc(hidden)]
    #[must_use]
    pub fn into_runtime(self) -> Runtime {
        self.handle.runtime
    }
}

/// Cloneable inspection and control handle for an active [`CodingSession`].
///
/// Designed to be shared across TUI effect workers, WebUI HTTP/WebSocket
/// handlers, and SDK orchestration tasks without exposing `merry-runtime`
/// internals.
#[derive(Clone)]
pub struct CodingSessionHandle {
    runtime: Runtime,
}

impl CodingSessionHandle {
    /// Creates a handle from a low-level runtime (internal escape hatch for
    /// fixtures and tests).
    #[doc(hidden)]
    #[must_use]
    pub fn from_runtime(runtime: Runtime) -> Self {
        Self { runtime }
    }

    /// Returns the session identifier.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        self.runtime.session_id()
    }

    /// Returns a read-only trajectory handle for observability and Web viewers.
    #[must_use]
    pub fn trajectory_handle(&self) -> TrajectoryHandle {
        TrajectoryHandle::from_runtime(self.runtime.clone())
    }

    /// Returns loaded skill metadata for this session.
    pub async fn skills(&self) -> Vec<SkillMetadata> {
        self.runtime.skills().await
    }

    /// Subscribes to live UI-only subagent activity snapshots.
    #[must_use]
    pub fn subscribe_subagent_activity(&self) -> SubagentActivityReceiver {
        self.runtime.subscribe_subagent_activity()
    }

    /// Returns aggregated session token usage, if any model turn has completed.
    pub async fn usage(&self) -> Option<SessionUsage> {
        self.runtime.usage().await
    }

    /// Returns the projected session transcript items.
    pub async fn session_transcript(&self) -> Result<Vec<SessionTranscriptItem>, RuntimeError> {
        self.runtime.session_transcript().await
    }

    /// Lists archived compacted segment numbers for lazy history loading.
    pub async fn archived_segment_numbers(&self) -> Result<Vec<u32>, RuntimeError> {
        self.runtime.archived_segment_numbers().await
    }

    /// Loads transcript items from one archived compacted segment.
    pub async fn read_archived_segment_transcript(
        &self,
        segment_number: u32,
    ) -> Result<Vec<SessionTranscriptItem>, RuntimeError> {
        self.runtime
            .read_archived_segment_transcript(segment_number)
            .await
    }

    /// Reads the full recorded output of a tool call.
    pub async fn read_tool_output(
        &self,
        call_id: &ToolCallId,
    ) -> Result<Option<ToolOutput>, RuntimeError> {
        self.runtime.read_tool_output(call_id).await
    }

    /// Returns the current Plan Mode snapshot.
    pub async fn plan_session_snapshot(&self) -> PlanSessionSnapshot {
        self.runtime.plan_session_snapshot().await
    }

    /// Returns all Markdown plan documents authored in this session.
    pub async fn plan_documents(&self) -> Vec<PlanDocument> {
        self.runtime.plan_documents().await
    }

    /// Returns the current session todo items.
    pub async fn todo_items(&self) -> Vec<TodoItem> {
        self.runtime.todo_items().await
    }

    /// Returns the already-cached session topic without waiting on background
    /// topic generation.
    pub async fn cached_session_topic(&self) -> Option<String> {
        self.runtime.cached_session_topic().await
    }

    /// Returns the session topic, awaiting any in-flight topic generation task.
    pub async fn session_topic(&self) -> Option<String> {
        self.runtime.session_topic().await
    }

    /// Returns current status views for all managed subagents, if subagents are
    /// configured.
    pub async fn subagent_snapshot(&self) -> Option<Vec<SubagentStatusView>> {
        self.runtime.subagent_snapshot().await
    }

    /// Opens an inspection handle (task, transcript, live event stream) for one
    /// subagent.
    pub async fn inspect_subagent(&self, agent_id: &SubagentId) -> Option<SubagentInspectSession> {
        self.runtime.inspect_subagent(agent_id).await
    }

    /// Sends a follow-up message to a managed subagent.
    pub async fn send_subagent_message(
        &self,
        agent_id: &SubagentId,
        message: String,
    ) -> Result<(), RuntimeError> {
        self.runtime.send_subagent_message(agent_id, message).await
    }

    /// Cancels a running subagent.
    pub async fn cancel_subagent(
        &self,
        agent_id: &SubagentId,
    ) -> Result<WaitSubagentsOutput, RuntimeError> {
        self.runtime.cancel_subagent(agent_id).await
    }

    /// Lists retained background processes owned by this session.
    #[must_use]
    pub fn background_processes(&self) -> Vec<BackgroundProcessSummary> {
        self.runtime.background_processes()
    }

    /// Reads captured output for one retained background process.
    pub fn read_background_process(
        &self,
        id: &BackgroundProcessId,
    ) -> Result<BackgroundProcessOutput, BackgroundProcessLookupError> {
        self.runtime.read_background_process(id)
    }

    /// Stops one retained background process group.
    pub fn stop_background_process(
        &self,
        id: &BackgroundProcessId,
    ) -> Result<BackgroundProcessStop, BackgroundProcessLookupError> {
        self.runtime.stop_background_process(id)
    }

    /// Marks any pending tool calls as failed so an interrupted session can be
    /// saved and safely resumed.
    pub async fn abandon_pending_tool_calls(&self, reason: &str) -> Result<usize, RuntimeError> {
        self.runtime.abandon_pending_tool_calls(reason).await
    }

    /// Saves the session state to `store`.
    pub async fn save_session_to(&self, store: FileSessionStore) -> Result<(), RuntimeError> {
        self.runtime.save_session_to(store).await
    }

    /// Starts a single-shot agent loop stream with an explicit loop config and
    /// generation settings.
    pub fn run_agent_loop_stream(
        &self,
        task: &str,
        generation: GenerationConfig,
        loop_config: AgentLoopConfig,
    ) -> Result<AgentRun, RuntimeError> {
        let input = StepInput::user_text(task)?;
        let context = StepContext::default().with_generation_config(generation);
        self.runtime
            .run_agent_loop_stream(input, context, loop_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_llm::{
        FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
        ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
        ModelStreamContext,
    };
    use merry_process::{LocalProcessBackend, ProcessBackendMode, ProcessBackendOptions};
    use std::sync::{Arc, OnceLock};

    struct StaticProvider;

    impl ModelProvider for StaticProvider {
        fn name(&self) -> &merry_core::ProviderName {
            static NAME: OnceLock<merry_core::ProviderName> = OnceLock::new();
            NAME.get_or_init(|| merry_core::ProviderName::new("coding-session-test").unwrap())
        }

        fn capabilities(&self) -> &ModelCapabilities {
            static CAPS: OnceLock<ModelCapabilities> = OnceLock::new();
            CAPS.get_or_init(|| {
                ModelCapabilities::new(true, true, false, true, None, None).unwrap()
            })
        }

        fn stream_model<'a>(
            &'a self,
            _request: ModelRequest,
            _context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            Box::pin(async move {
                let response =
                    ModelResponse::new(vec![ModelOutput::text("ok")], FinishReason::Stop, None);
                let stream: ModelEventStream = Box::pin(futures_util::stream::iter([
                    Ok(ModelEvent::Started),
                    Ok(ModelEvent::Completed { response }),
                ]));
                Ok(stream)
            })
        }
    }

    fn test_input(session_id: &str, workspace: &std::path::Path) -> CodingRuntimeInput {
        let backend = Arc::new(
            LocalProcessBackend::new(
                workspace,
                ProcessBackendMode::Host,
                ProcessBackendOptions::new(),
            )
            .expect("host backend should build"),
        );
        CodingRuntimeInput::new(
            SessionId::new(session_id).expect("valid session id"),
            workspace,
            Arc::new(StaticProvider),
            ModelName::new("test-model").expect("valid model"),
            backend,
        )
    }

    #[tokio::test]
    async fn coding_session_open_enforces_lock_and_new_collision_and_resume() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let store = FileSessionStore::new(temp.path().join("sessions"));

        // 1. Open new session, hold lock, verify concurrent resume returns AlreadyOpen.
        let session = CodingSession::builder(test_input("webui-session-1", &workspace))
            .open(store.clone(), CodingSessionOpenMode::New)
            .await
            .expect("new coding session should open");
        assert_eq!(session.session_id().as_str(), "webui-session-1");
        assert_eq!(
            session.trajectory_handle().session_id().as_str(),
            "webui-session-1"
        );

        let locked = CodingSession::builder(test_input("webui-session-1", &workspace))
            .open(store.clone(), CodingSessionOpenMode::Resume)
            .await;
        assert!(matches!(
            locked,
            Err(CodingSessionOpenError::ResumeUnavailable {
                kind: CodingSessionResumeFailure::AlreadyOpen,
                ..
            })
        ));

        // 2. Save session, release lock, verify New refuses existing state and Resume succeeds.
        session
            .handle()
            .save_session_to(store.clone())
            .await
            .expect("session should save");
        drop(session);

        let collision = CodingSession::builder(test_input("webui-session-1", &workspace))
            .open(store.clone(), CodingSessionOpenMode::New)
            .await;
        assert!(matches!(
            collision,
            Err(CodingSessionOpenError::SessionAlreadyExists { .. })
        ));

        let resumed = CodingSession::builder(test_input("webui-session-1", &workspace))
            .open(store.clone(), CodingSessionOpenMode::Resume)
            .await
            .expect("saved session should resume");
        let interactive = resumed
            .start_interactive(GenerationConfig::default())
            .await
            .expect("interactive run should start");
        assert!(interactive.skills.is_empty());
    }

    #[test]
    fn classify_resume_error_matches_locked_and_corrupt_store_errors() {
        assert_eq!(
            classify_resume_error(&RuntimeError::SessionStore {
                source: SessionStoreError::UnsupportedFormatVersion { actual: 6 },
            }),
            Some(CodingSessionResumeFailure::IncompatibleFormat)
        );
        assert_eq!(
            classify_resume_error(&RuntimeError::SessionStore {
                source: SessionStoreError::InvalidDocument {
                    reason: "legacy journal",
                },
            }),
            Some(CodingSessionResumeFailure::IncompatibleFormat)
        );
        assert_eq!(
            classify_resume_error(&RuntimeError::SessionStore {
                source: SessionStoreError::SessionAlreadyReserved {
                    session_id: SessionId::new("busy").expect("valid id"),
                },
            }),
            Some(CodingSessionResumeFailure::AlreadyOpen)
        );
    }

    #[tokio::test]
    async fn coding_session_discard_if_empty_removes_untouched_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let store = FileSessionStore::new(temp.path().join("sessions"));
        let session_id = SessionId::new("untouched-session").expect("valid session id");

        let mut session = CodingSession::builder(test_input("untouched-session", &workspace))
            .open(store.clone(), CodingSessionOpenMode::New)
            .await
            .expect("session should open");
        assert!(store.session_dir(&session_id).exists());

        session.discard_if_empty().await;
        assert!(!store.session_dir(&session_id).exists());
    }
}
