use crate::cli_error::{CliError, debug_openai_usage_error, unexpected};
use crate::coding::{
    AutoFallback, CodingPermissionPolicy, HeadlessCodingRuntimeInput, PermissionMode, SandboxMode,
    action_process_runner_for_mode, build_headless_coding_with_policy_composition,
    coding_agent_process_admission, resume_headless_coding_composition_with_loaded_session,
};
use crate::config::MerryConfig;
use crate::mcp_tools::discover_configured_mcp_tools;
use crate::provider_config::{
    RuntimePrimaryProviderConfig, RuntimeProviderBundle, RuntimeRoleOverrides,
    build_coding_subagents_config, runtime_provider_bundle_from_config_with_overrides,
};
use crate::runtime_config::{
    automatic_compaction_config, main_reasoning_effort,
    prepared_action_process_backend_options_with_overrides, private_tool_paths, subagents_config,
};
use crate::tui::preferences::{CompactionStrategy, TuiPreferences};
use crate::tui::session_list::{TuiSessionMetadata, TuiSessionStore, now_unix_ms};
use crate::tui::session_picker::SessionPickerSelection;
use merry_core::SessionId;
use merry_llm::GenerationConfig;
use merry_mcp::McpServerDiagnostic;
use merry_runtime::{
    AgentLoopControl, AgentLoopInput, ChannelPermissionAdmissionSource, CompactionConfig,
    InteractivePrimaryModel, InteractiveRunEventStream, InteractiveSettingsUpdate,
    InteractiveSubagentSettings, LoadedSession, PermissionReviewRequest, Runtime,
    SessionReservation, SessionTranscriptItem, SkillMetadata, StepContext,
    SubagentActivityReceiver,
};
use std::{collections::VecDeque, num::NonZeroU64, path::PathBuf, sync::Arc};
use tokio::sync::mpsc;

pub(crate) struct TuiRuntimeSession {
    pub(crate) workspace_root: PathBuf,
    pub(crate) model_label: String,
    pub(crate) reasoning_effort_label: Option<String>,
    pub(crate) metadata: TuiSessionMetadata,
    metadata_written: bool,
    pub(crate) resumed: bool,
    pub(crate) startup_warnings: Vec<McpServerDiagnostic>,
    runtime: Runtime,
    session_store: TuiSessionStore,
    pub(crate) stream: InteractiveRunEventStream,
    pub(crate) subagent_activity: SubagentActivityReceiver,
    pub(crate) input: AgentLoopInput,
    pub(crate) control: AgentLoopControl,
    pub(crate) skills: Vec<SkillMetadata>,
    merry_config: MerryConfig,
    explicit_cli_flags: crate::cli::ExplicitCliFlags,
    configured_permission_mode: PermissionMode,
    configured_auto_fallback: AutoFallback,
    permission_source: Arc<ChannelPermissionAdmissionSource>,
    pub(crate) permission_requests: mpsc::Receiver<PermissionReviewRequest>,
    pending_permission_reviews: VecDeque<PermissionReviewRequest>,
    session_reservation: Option<SessionReservation>,
}

pub(crate) struct TuiSessionLaunch {
    pub(crate) sandbox_mode: SandboxMode,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) auto_fallback: AutoFallback,
    pub(crate) configured_permission_mode: PermissionMode,
    pub(crate) configured_auto_fallback: AutoFallback,
    pub(crate) explicit_cli_flags: crate::cli::ExplicitCliFlags,
    pub(crate) workspace_root: PathBuf,
    pub(crate) workspace_writable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeFailureKind {
    IncompatibleFormat,
    AlreadyOpen,
}

pub(crate) enum TuiSessionStartError {
    ResumeUnavailable {
        kind: ResumeFailureKind,
        session_id: SessionId,
        session_dir: PathBuf,
    },
    Fatal(CliError),
}

impl From<CliError> for TuiSessionStartError {
    fn from(error: CliError) -> Self {
        Self::Fatal(error)
    }
}

impl From<crate::coding::CodingRuntimeError> for TuiSessionStartError {
    fn from(error: crate::coding::CodingRuntimeError) -> Self {
        Self::Fatal(CliError::from(error))
    }
}

pub(super) fn classify_resume_error(
    error: &merry_runtime::RuntimeError,
) -> Option<ResumeFailureKind> {
    match error {
        merry_runtime::RuntimeError::SessionStore {
            source: merry_runtime::SessionStoreError::SessionAlreadyReserved { .. },
        } => Some(ResumeFailureKind::AlreadyOpen),
        merry_runtime::RuntimeError::SessionStore {
            source:
                merry_runtime::SessionStoreError::UnsupportedFormatVersion { .. }
                | merry_runtime::SessionStoreError::InvalidDocument { .. }
                | merry_runtime::SessionStoreError::Json { .. }
                | merry_runtime::SessionStoreError::SessionIdMismatch { .. }
                | merry_runtime::SessionStoreError::Io { .. },
        } => Some(ResumeFailureKind::IncompatibleFormat),
        _ => None,
    }
}

pub(crate) async fn start_tui_runtime_session(
    merry_config: Option<&MerryConfig>,
    session_store: TuiSessionStore,
    selection: SessionPickerSelection,
    preferences: &TuiPreferences,
    launch: TuiSessionLaunch,
) -> Result<TuiRuntimeSession, TuiSessionStartError> {
    let TuiSessionLaunch {
        sandbox_mode,
        permission_mode,
        auto_fallback,
        configured_permission_mode,
        configured_auto_fallback,
        explicit_cli_flags,
        workspace_root,
        workspace_writable,
    } = launch;
    let _admission = coding_agent_process_admission(sandbox_mode);
    let owned_config = merry_config.cloned().ok_or_else(|| {
        debug_openai_usage_error("Merry XDG provider config is required for runtime".to_owned())
    })?;

    let role_overrides = runtime_role_overrides(merry_config, preferences);
    let RuntimeProviderBundle {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    } = runtime_provider_bundle_from_config_with_overrides(
        merry_config,
        preferences.provider.as_deref(),
        preference_model_override(&owned_config, preferences)?,
        role_overrides,
        debug_openai_usage_error,
    )?;
    let RuntimePrimaryProviderConfig { provider, model } = primary;
    let model_label = model.as_str().to_owned();
    let generation =
        generation_config_with_preferences(merry_config, preferences).map_err(unexpected)?;
    let reasoning_effort_label = generation
        .reasoning_effort()
        .map(|effort| effort.as_str().to_owned());
    let (session_id, mut metadata, should_resume) = session_start(selection, &workspace_root);
    let state_store = session_store.session_state_store();
    let session_reservation = match state_store.reserve_session(&session_id).await {
        Ok(reservation) => reservation,
        Err(merry_runtime::SessionStoreError::SessionAlreadyReserved { .. }) if should_resume => {
            tracing::warn!(
                session_id = %session_id,
                "cannot resume session because it is already reserved by another run"
            );
            return Err(TuiSessionStartError::ResumeUnavailable {
                kind: ResumeFailureKind::AlreadyOpen,
                session_dir: state_store.session_dir(&session_id),
                session_id,
            });
        }
        Err(error) => return Err(TuiSessionStartError::Fatal(unexpected(error))),
    };
    metadata.model = Some(model_label.clone());
    metadata.reasoning_effort = reasoning_effort_label.clone();
    let mut backend_options = prepared_action_process_backend_options_with_overrides(
        merry_config,
        sandbox_mode,
        &preferences.permissions,
    )
    .await?;
    if !workspace_writable {
        backend_options = backend_options.read_only_workspace();
    }
    let backend = action_process_runner_for_mode(&workspace_root, backend_options, sandbox_mode)?;
    let (permission_source, permission_requests) = ChannelPermissionAdmissionSource::channel(8);
    let permission_source = Arc::new(permission_source);
    let loaded_session = if should_resume {
        match LoadedSession::load(&state_store, &session_id).await {
            Ok(loaded) => Some(loaded),
            Err(error) => {
                if let Some(kind) = classify_resume_error(&error) {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %error,
                        "cannot resume historical session directly"
                    );
                    return Err(TuiSessionStartError::ResumeUnavailable {
                        kind,
                        session_dir: state_store.session_dir(&session_id),
                        session_id,
                    });
                }
                return Err(TuiSessionStartError::Fatal(unexpected(format!(
                    "could not resume session {session_id}: {error}"
                ))));
            }
        }
    } else {
        None
    };
    let mcp = discover_configured_mcp_tools(merry_config).await?;
    let mut subagents = subagents_config(merry_config)
        .map_err(unexpected)?
        .with_overrides(
            preferences.subagents_enabled,
            preferences.subagent_max_threads,
            preferences.subagent_limit_behavior.map(Into::into),
        )
        .map_err(unexpected)?;
    subagents
        .apply_tui_overrides(
            preferences
                .subagent_default_model
                .as_ref()
                .and_then(|binding| binding.provider.as_deref()),
            preferences
                .subagent_default_model
                .as_ref()
                .and_then(|binding| binding.model.as_deref()),
            preferences
                .subagent_default_model
                .as_ref()
                .and_then(|binding| binding.reasoning_effort.clone()),
            &preferences.subagent_roles,
        )
        .map_err(unexpected)?;
    let coding_subagents = build_coding_subagents_config(
        merry_config,
        &subagents,
        &workspace_root,
        true,
        debug_openai_usage_error,
    )?;
    let history_retention =
        crate::runtime_config::history_retention_config(merry_config).map_err(unexpected)?;
    let runtime_input = HeadlessCodingRuntimeInput {
        session_id: session_id.as_str(),
        root: &workspace_root,
        provider,
        model,
        process_backend: backend,
        extra_tools: mcp.tools,
        automatic_compaction: automatic_compaction_config_with_preferences(
            merry_config,
            preferences,
        )?,
        history_retention,
        retry_policy,
        context_compaction,
        approval_review,
        skill_roots: merry_config
            .map(MerryConfig::skill_roots)
            .transpose()
            .map_err(unexpected)?
            .unwrap_or_default(),
        denied_absolute_paths: private_tool_paths(merry_config).map_err(unexpected)?,
        subagents: coding_subagents,
        workspace_tool_limits: None,
        workspace_writable,
    };
    let permission_policy = CodingPermissionPolicy::for_permission_mode(
        permission_mode.coding_mode(),
        auto_fallback.coding_fallback(),
        Some(Arc::clone(&permission_source) as Arc<dyn merry_runtime::PermissionAdmissionSource>),
    )
    .map_err(unexpected)?;
    let permission_review_rules = merry_config
        .map(MerryConfig::permission_review_rules)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let max_parallel_tool_calls = preferences
        .max_parallel_tool_calls
        .and_then(std::num::NonZeroUsize::new)
        .or(merry_config
            .map(MerryConfig::max_parallel_tool_calls)
            .transpose()
            .map_err(unexpected)?
            .flatten());
    let coding_runtime = match loaded_session {
        Some(loaded) => resume_headless_coding_composition_with_loaded_session(
            runtime_input,
            loaded,
            permission_policy,
            permission_review_rules,
            Some(state_store),
            max_parallel_tool_calls,
        )
        .map_err(CliError::from)?,
        None => build_headless_coding_with_policy_composition(
            runtime_input,
            permission_policy,
            permission_review_rules,
            Some(state_store),
            max_parallel_tool_calls,
        )
        .map_err(CliError::from)?,
    };
    let loop_config = coding_runtime.loop_config();
    let runtime = coding_runtime.into_runtime();
    let skills = runtime.skills().await;
    let interactive = runtime
        .start_interactive_agent_run(
            StepContext::new(Default::default()).with_generation_config(generation),
            loop_config,
        )
        .map_err(unexpected)?;
    let subagent_activity = runtime.subscribe_subagent_activity();
    let (stream, input, control) = interactive.split();
    let mut startup_update = InteractiveSettingsUpdate::default();
    let mut needs_startup_update = false;
    if preferences.context_window_tokens.is_some() {
        startup_update = startup_update
            .with_context_window_tokens(context_window_tokens_with_preferences(preferences)?);
        needs_startup_update = true;
    }
    if preferences
        .context_compaction_model
        .as_ref()
        .is_some_and(|b| b.reasoning_effort.is_some())
        || preferences
            .approval_review_model
            .as_ref()
            .is_some_and(|b| b.reasoning_effort.is_some())
    {
        let role_overrides = runtime_role_overrides(Some(&owned_config), preferences);
        let bundle = runtime_provider_bundle_from_config_with_overrides(
            Some(&owned_config),
            preferences.provider.as_deref(),
            preference_model_override(&owned_config, preferences)?,
            role_overrides,
            debug_openai_usage_error,
        )?;
        startup_update = attach_live_role_models(startup_update, &bundle, preferences);
        needs_startup_update = true;
    }
    if needs_startup_update {
        control
            .update_settings(startup_update)
            .await
            .map_err(unexpected)?;
    }

    Ok(TuiRuntimeSession {
        workspace_root,
        model_label,
        reasoning_effort_label,
        metadata,
        metadata_written: should_resume,
        resumed: should_resume,
        startup_warnings: mcp.warnings,
        runtime,
        session_store,
        stream,
        subagent_activity,
        input,
        control,
        skills,
        merry_config: owned_config,
        explicit_cli_flags,
        configured_permission_mode,
        configured_auto_fallback,
        permission_source,
        permission_requests,
        pending_permission_reviews: VecDeque::new(),
        session_reservation: Some(session_reservation),
    })
}

impl TuiRuntimeSession {
    pub(crate) fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub(crate) fn pending_permission_review_count(&self) -> usize {
        self.pending_permission_reviews.len()
    }

    pub(crate) fn enqueue_permission_review(
        &mut self,
        request: PermissionReviewRequest,
    ) -> Option<crate::permission_prompt::PermissionReviewView> {
        if request.is_cancelled() {
            return None;
        }
        let should_open = self.pending_permission_reviews.is_empty();
        self.pending_permission_reviews.push_back(request);
        let total = self.pending_permission_reviews.len();
        should_open
            .then(|| {
                self.pending_permission_reviews
                    .front()
                    .map(|front| permission_review_view(front, total))
            })
            .flatten()
    }

    pub(crate) fn prune_cancelled_permission_reviews(
        &mut self,
    ) -> Option<Option<crate::permission_prompt::PermissionReviewView>> {
        let mut pruned = false;
        while self
            .pending_permission_reviews
            .front()
            .is_some_and(PermissionReviewRequest::is_cancelled)
        {
            self.pending_permission_reviews.pop_front();
            pruned = true;
        }
        pruned.then(|| {
            let total = self.pending_permission_reviews.len();
            self.pending_permission_reviews
                .front()
                .map(|front| permission_review_view(front, total))
        })
    }

    pub(crate) fn resolve_permission_review(
        &mut self,
        approval_id: &str,
        approved: bool,
    ) -> Result<Option<crate::permission_prompt::PermissionReviewView>, CliError> {
        let Some(request) = self.pending_permission_reviews.pop_front() else {
            return Ok(None);
        };
        if request.approval_id() != approval_id {
            self.pending_permission_reviews.push_front(request);
            return Err(unexpected(
                "permission review overlay did not match pending request",
            ));
        }
        let result = if approved {
            request.approve("Approved by the Merry permission review UI.")
        } else {
            request.deny("Rejected by the Merry permission review UI.")
        };
        result.map_err(unexpected)?;
        let total = self.pending_permission_reviews.len();
        Ok(self
            .pending_permission_reviews
            .front()
            .map(|front| permission_review_view(front, total)))
    }

    pub(crate) fn replace_config(&mut self, config: MerryConfig) {
        self.merry_config = config;
    }

    pub(crate) async fn apply_preferences(
        &mut self,
        preferences: &TuiPreferences,
    ) -> Result<(), CliError> {
        let role_overrides = runtime_role_overrides(Some(&self.merry_config), preferences);
        let bundle = runtime_provider_bundle_from_config_with_overrides(
            Some(&self.merry_config),
            preferences.provider.as_deref(),
            preference_model_override(&self.merry_config, preferences)?,
            role_overrides,
            debug_openai_usage_error,
        )?;
        let provider = Arc::clone(&bundle.primary.provider);
        let model = bundle.primary.model.clone();
        let retry_policy = bundle.retry_policy.unwrap_or_default();
        let generation = generation_config_with_preferences(Some(&self.merry_config), preferences)
            .map_err(unexpected)?;
        let subagents = self
            .merry_config
            .subagents_config()
            .map_err(unexpected)?
            .with_overrides(
                preferences.subagents_enabled,
                preferences.subagent_max_threads,
                preferences.subagent_limit_behavior.map(Into::into),
            )
            .map_err(unexpected)?;
        let automatic_compaction =
            automatic_compaction_config_with_preferences(Some(&self.merry_config), preferences)?;
        let max_parallel_tool_calls = preferences
            .max_parallel_tool_calls
            .and_then(std::num::NonZeroUsize::new)
            .or(self
                .merry_config
                .max_parallel_tool_calls()
                .map_err(unexpected)?)
            .unwrap_or(std::num::NonZeroUsize::new(8).expect("default parallel limit is non-zero"));
        let effective_permission_mode = crate::tui::resolve_effective_permission_mode(
            self.explicit_cli_flags,
            preferences,
            self.configured_permission_mode,
        );
        let effective_auto_fallback = crate::tui::resolve_effective_auto_fallback(
            self.explicit_cli_flags,
            preferences,
            self.configured_auto_fallback,
        );
        let review_mode =
            permission_review_mode_for(effective_permission_mode, effective_auto_fallback);

        let mut update = InteractiveSettingsUpdate::default()
            .with_primary_model(InteractivePrimaryModel::new(
                provider,
                model.clone(),
                retry_policy,
            ))
            .with_generation_config(generation.clone())
            .with_subagents(InteractiveSubagentSettings::new(
                subagents.is_enabled(),
                subagents.limits(),
            ))
            .with_automatic_compaction(automatic_compaction)
            .with_context_window_tokens(context_window_tokens_with_preferences(preferences)?)
            .with_max_parallel_tool_calls(max_parallel_tool_calls)
            .with_permission_admission_source(Arc::clone(&self.permission_source)
                as Arc<dyn merry_runtime::PermissionAdmissionSource>)
            .with_permission_review_mode(review_mode);
        update = attach_live_role_models(update, &bundle, preferences);
        self.control
            .update_settings(update)
            .await
            .map_err(unexpected)?;

        self.model_label = model.as_str().to_owned();
        self.reasoning_effort_label = generation
            .reasoning_effort()
            .map(|effort| effort.as_str().to_owned());
        self.metadata.model = Some(self.model_label.clone());
        self.metadata.reasoning_effort = self.reasoning_effort_label.clone();
        Ok(())
    }

    pub(crate) fn set_topic_if_empty(&mut self, topic: Option<String>) {
        self.metadata.set_topic_if_empty(topic);
    }

    pub(crate) fn is_metadata_written(&self) -> bool {
        self.metadata_written
    }

    /// Submits a user message to the interactive runtime and writes initial
    /// picker metadata (`meta.json`) on first activity so a mid-turn crash
    /// after runtime journal savepoints land remains resumable.
    pub(crate) async fn submit_message(
        &mut self,
        message: merry_runtime::UserMessageInput,
        fallback_text: &str,
    ) -> Result<(), merry_runtime::InteractiveError> {
        self.input.submit_message(message).await?;
        let fallback_topic = if self.metadata.topic.is_none() {
            let topic = crate::tui::session_list::compact_session_text(fallback_text);
            (!topic.is_empty()).then_some(topic)
        } else {
            None
        };
        self.ensure_metadata_written(fallback_topic).await;
        Ok(())
    }

    /// Writes `meta.json` on first user submission (or once a fallback topic
    /// becomes available) without creating metadata for untouched empty sessions.
    pub(crate) async fn ensure_metadata_written(&mut self, fallback_topic: Option<String>) {
        let had_topic = self.metadata.topic.is_some();
        self.metadata.set_topic_if_empty(fallback_topic);
        let topic_populated = !had_topic && self.metadata.topic.is_some();
        if self.metadata_written && !topic_populated {
            return;
        }
        self.metadata.mark_active(now_unix_ms());
        match write_session_metadata(&self.session_store, &self.metadata).await {
            Ok(()) => {
                self.metadata_written = true;
            }
            Err(error) => {
                tracing::warn!(
                    session_id = %self.metadata.session_id,
                    error = ?error,
                    "could not write initial TUI session metadata"
                );
            }
        }
    }

    /// Refreshes picker metadata after an automatic runtime turn savepoint
    /// without blocking the TUI loop on background topic generation.
    pub(crate) async fn sync_metadata_on_turn_settled(&mut self, fallback_topic: Option<String>) {
        if let Some(topic) = self.runtime.cached_session_topic().await {
            self.metadata.topic = Some(topic);
        } else {
            self.metadata.set_topic_if_empty(fallback_topic);
        }
        self.metadata.mark_active(now_unix_ms());
        match write_session_metadata(&self.session_store, &self.metadata).await {
            Ok(()) => {
                self.metadata_written = true;
            }
            Err(error) => {
                tracing::warn!(
                    session_id = %self.metadata.session_id,
                    error = ?error,
                    "could not refresh TUI session metadata after turn"
                );
            }
        }
    }

    pub(crate) async fn save_on_exit(&mut self) -> Result<(), CliError> {
        self.apply_generated_topic().await;
        self.metadata.mark_active(now_unix_ms());
        self.runtime
            .save_session_to(self.session_store.session_state_store())
            .await
            .map_err(unexpected)?;
        write_session_metadata(&self.session_store, &self.metadata).await?;
        self.metadata_written = true;
        Ok(())
    }

    /// Returns true when this session has resumed history, already persisted a
    /// journal (for example via `/save`), or recorded at least one transcript item.
    pub(crate) async fn has_persistable_activity(&self) -> bool {
        if self.resumed {
            return true;
        }
        if self
            .session_store
            .session_state_store()
            .contains_session(&self.metadata.session_id)
            .await
            .unwrap_or(false)
        {
            return true;
        }
        self.runtime
            .session_transcript()
            .await
            .is_ok_and(|items| !items.is_empty())
    }

    /// Releases the reservation and removes the session directory when an
    /// untouched new session exits without writing any persisted files.
    pub(crate) async fn discard_if_empty(&mut self) {
        if let Some(reservation) = self.session_reservation.take() {
            self.session_store
                .session_state_store()
                .release_empty_reservation(&self.metadata.session_id, reservation)
                .await;
        }
    }

    pub(crate) async fn save_now(&mut self) -> Result<(), CliError> {
        self.apply_generated_topic().await;
        self.metadata.mark_active(now_unix_ms());
        self.control
            .save_session_to(self.session_store.session_state_store())
            .await
            .map_err(unexpected)?;
        write_session_metadata(&self.session_store, &self.metadata).await?;
        self.metadata_written = true;
        Ok(())
    }

    async fn apply_generated_topic(&mut self) {
        if let Some(topic) = self.runtime.session_topic().await {
            self.metadata.topic = Some(topic);
        }
    }

    pub(crate) async fn session_transcript(&self) -> Result<Vec<SessionTranscriptItem>, CliError> {
        self.runtime.session_transcript().await.map_err(unexpected)
    }

    pub(crate) async fn plan_session_snapshot(&self) -> merry_core::PlanSessionSnapshot {
        self.runtime.plan_session_snapshot().await
    }

    pub(crate) async fn plan_documents(&self) -> Vec<merry_core::PlanDocument> {
        self.runtime.plan_documents().await
    }

    pub(crate) async fn todo_items(&self) -> Vec<merry_core::TodoItem> {
        self.runtime.todo_items().await
    }
}

async fn write_session_metadata(
    store: &TuiSessionStore,
    metadata: &TuiSessionMetadata,
) -> Result<(), CliError> {
    let stored = store.clone();
    let recorded = metadata.clone();
    tokio::task::spawn_blocking(move || stored.write_metadata(&recorded))
        .await
        .map_err(unexpected)?
        .map_err(unexpected)?;
    store
        .session_state_store()
        .update_session_search_labels(&metadata.session_id, metadata.topic.as_deref())
        .await;
    Ok(())
}

fn permission_review_view(
    request: &PermissionReviewRequest,
    queue_total: usize,
) -> crate::permission_prompt::PermissionReviewView {
    crate::permission_prompt::PermissionReviewView::from_request(request, queue_total)
}

fn preference_model_override<'a>(
    config: &MerryConfig,
    preferences: &'a TuiPreferences,
) -> Result<Option<&'a str>, CliError> {
    let provider = match preferences.provider.as_deref() {
        Some(provider) => provider.to_owned(),
        None => config.default_provider().map_err(unexpected)?.alias,
    };
    Ok(preferences.model_for_provider(&provider))
}

fn session_start(
    selection: SessionPickerSelection,
    workspace_root: &std::path::Path,
) -> (SessionId, TuiSessionMetadata, bool) {
    let now = now_unix_ms();
    match selection {
        SessionPickerSelection::Resume(mut metadata) => {
            metadata.mark_active(now);
            (metadata.session_id.clone(), metadata, true)
        }
        SessionPickerSelection::New | SessionPickerSelection::Quit => {
            let session_id = default_tui_session_id();
            let metadata =
                TuiSessionMetadata::new(session_id.clone(), workspace_root.to_path_buf(), now);
            (session_id, metadata, false)
        }
    }
}

pub(crate) fn default_tui_session_id() -> merry_core::SessionId {
    crate::session_id::new_ephemeral_session_id()
}

fn generation_config_with_preferences(
    merry_config: Option<&MerryConfig>,
    preferences: &TuiPreferences,
) -> Result<GenerationConfig, crate::config::ConfigError> {
    let selected_provider = match preferences.provider.as_deref() {
        Some(provider) => Some(provider.to_owned()),
        None => merry_config
            .map(MerryConfig::configured_default_provider)
            .transpose()?
            .flatten()
            .map(|provider| provider.alias),
    };
    let preference_reasoning_effort = selected_provider
        .as_deref()
        .and_then(|provider| preferences.reasoning_effort_for_provider(provider))
        .cloned();
    let provider_reasoning_effort = merry_config
        .zip(selected_provider.as_deref())
        .map(|(config, provider)| config.effective_provider_reasoning_effort(provider))
        .transpose()?
        .flatten();
    let reasoning_effort = preference_reasoning_effort
        .or(provider_reasoning_effort)
        .or(main_reasoning_effort(merry_config)?);
    // The service tier is provider-specific, so the selected provider owns it.
    // It is never inherited from [providers.default] when that default names a
    // different provider, which may not accept the tier at all.
    let preference_service_tier = selected_provider
        .as_deref()
        .and_then(|provider| preferences.service_tier_for_provider(provider))
        .filter(|tier| {
            let Some((config, provider)) = merry_config.zip(selected_provider.as_deref()) else {
                return true;
            };
            config
                .provider_profile(provider)
                .ok()
                .is_some_and(|profile| {
                    profile.kind() == crate::config::ConfiguredProviderKind::OpenAiCompatible
                        && profile
                            .protocol()
                            .unwrap_or_default()
                            .supports_service_tier(*tier)
                })
        });
    let provider_service_tier = merry_config
        .zip(selected_provider.as_deref())
        .map(|(config, provider)| config.effective_provider_service_tier(provider))
        .transpose()?
        .flatten();
    let service_tier = preference_service_tier.or(provider_service_tier);
    Ok(GenerationConfig::default()
        .with_reasoning_effort(reasoning_effort)
        .with_service_tier(service_tier))
}

fn automatic_compaction_config_with_preferences(
    merry_config: Option<&MerryConfig>,
    preferences: &TuiPreferences,
) -> Result<CompactionConfig, CliError> {
    let inherited = automatic_compaction_config(merry_config).map_err(unexpected)?;
    let mut policy = match preferences.compaction_strategy {
        None | Some(CompactionStrategy::Balanced) => inherited.policy(),
        Some(CompactionStrategy::Compact) => inherited
            .policy()
            .with_retained_model_turns(3)
            .map_err(unexpected)?,
        Some(CompactionStrategy::PreserveDetail) => inherited
            .policy()
            .with_retained_model_turns(7)
            .map_err(unexpected)?,
    };
    if let Some(turns) = preferences.retained_model_turns {
        policy = policy
            .with_retained_model_turns(turns)
            .map_err(unexpected)?;
    }
    let enabled = preferences
        .auto_compaction_enabled
        .unwrap_or_else(|| inherited.is_enabled());
    Ok(if enabled {
        CompactionConfig::enabled(policy)
    } else {
        CompactionConfig::disabled()
    })
}

fn context_window_tokens_with_preferences(
    preferences: &TuiPreferences,
) -> Result<Option<NonZeroU64>, CliError> {
    preferences
        .context_window_tokens
        .map(|tokens| {
            NonZeroU64::new(tokens)
                .ok_or_else(|| unexpected("context window preference must be greater than zero"))
        })
        .transpose()
}

fn permission_review_mode_for(
    mode: PermissionMode,
    fallback: AutoFallback,
) -> merry_runtime::PermissionReviewMode {
    match mode {
        PermissionMode::AlwaysApprove => merry_runtime::PermissionReviewMode::FullyTrusted,
        PermissionMode::Deny => merry_runtime::PermissionReviewMode::DenyAll,
        PermissionMode::Ask => merry_runtime::PermissionReviewMode::HostDecisionOnly,
        PermissionMode::Auto => match fallback {
            AutoFallback::Ask => merry_runtime::PermissionReviewMode::ModelThenHostFallback,
            AutoFallback::Deny => merry_runtime::PermissionReviewMode::Required,
        },
    }
}

fn attach_live_role_models(
    mut update: InteractiveSettingsUpdate,
    bundle: &RuntimeProviderBundle,
    preferences: &TuiPreferences,
) -> InteractiveSettingsUpdate {
    let retry_policy = bundle.retry_policy.unwrap_or_default();
    let build_role = |role_cfg: Option<&merry::profiles::CodingModelRoleConfig>,
                      effort: Option<merry_llm::ReasoningEffort>|
     -> Option<InteractivePrimaryModel> {
        if let Some(cfg) = role_cfg {
            Some(
                InteractivePrimaryModel::new(cfg.provider(), cfg.model().clone(), retry_policy)
                    .with_reasoning_effort(effort),
            )
        } else if effort.is_some() {
            Some(
                InteractivePrimaryModel::new(
                    Arc::clone(&bundle.primary.provider),
                    bundle.primary.model.clone(),
                    retry_policy,
                )
                .with_reasoning_effort(effort),
            )
        } else {
            None
        }
    };
    let compaction_effort = preferences
        .context_compaction_model
        .as_ref()
        .and_then(|binding| binding.reasoning_effort.clone());
    let approval_effort = preferences
        .approval_review_model
        .as_ref()
        .and_then(|binding| binding.reasoning_effort.clone());
    update = update.with_role_model(
        merry_runtime::RuntimeModelRole::ContextCompaction,
        build_role(bundle.context_compaction.as_ref(), compaction_effort),
    );
    update = update.with_role_model(
        merry_runtime::RuntimeModelRole::ApprovalReview,
        build_role(bundle.approval_review.as_ref(), approval_effort),
    );
    update
}

fn runtime_role_overrides<'a>(
    merry_config: Option<&MerryConfig>,
    preferences: &'a TuiPreferences,
) -> RuntimeRoleOverrides<'a> {
    let valid_role = |binding: Option<&'a crate::tui::preferences::RoleModelOverride>| {
        let binding = binding?;
        let provider = binding.provider.as_deref()?;
        let model = binding.model.as_deref()?;
        if merry_config.is_some_and(|config| config.provider_profile(provider).is_err()) {
            return None;
        }
        Some((provider, model))
    };
    RuntimeRoleOverrides {
        context_compaction: valid_role(preferences.context_compaction_model.as_ref()),
        approval_review: valid_role(preferences.approval_review_model.as_ref()),
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
