use super::config::CompactionConfig;
use super::state::AcceptedLocalWorkspaceProcessRunner;
use super::{Runtime, RuntimeInner};
use crate::{
    AcceptedLocalWorkspaceProcessAdmission, CompactedCheckpoint, FileSessionStore, LoadedSession,
    ProcessRunner, ProjectRules, PromptProfile, RuntimeCapabilities, RuntimeError,
    RuntimeModelRole, RuntimeProfile, SkillCatalog, TaskAnchor, ToolAdmission,
    memory::{MemoryActivationSource, StoredMemoryActivationSource},
    model_config::RuntimeModelConfigs,
    permission::{
        PermissionAdmissionSource, PermissionReviewMode, PermissionReviewRules, RuntimeTrustLevel,
    },
    plan::tools::coordinator_plan_registered_tools,
    process::{
        BackgroundProcessControl, PermissionedProcessRunnerFactory,
        StaticPermissionedProcessRunnerFactory,
    },
    session::SessionState,
    subagent::{
        ChildWorkspaceScope, SUBAGENT_WORKSPACE_SCOPE_CONTEXT_ID, SubagentActivityHub,
        SubagentManager,
    },
    todo::todo_registered_tool,
    tool::{RegisteredTool, ToolRegistry, ToolRegistryError},
    trajectory::RuntimeObservability,
};
use merry_core::{SessionId, ToolName};
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use std::{
    collections::{BTreeMap, VecDeque},
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
};
use tokio::sync::{Mutex, RwLock};

const DEFAULT_EVENT_BUFFER_SIZE: usize = 16;
const DEFAULT_MAX_PARALLEL_TOOL_CALLS: usize = 8;

/// Builder for a Merry runtime.
///
/// The builder wires provider-neutral runtime configuration: event buffering,
/// one optional model provider, and zero or more runtime-owned tool executors.
/// Provider integrations stay outside this crate behind [`ModelProvider`].
pub struct RuntimeBuilder {
    session_id: SessionId,
    prompt_cache_key: Option<SessionId>,
    event_buffer_size: NonZeroUsize,
    max_parallel_tool_calls: NonZeroUsize,
    model_configs: RuntimeModelConfigs,
    model_retry_policy: ModelRetryPolicy,
    automatic_compaction: CompactionConfig,
    capabilities: RuntimeCapabilities,
    prompt_profile: PromptProfile,
    progress_commentary: bool,
    registered_tools: Vec<RegisteredTool>,
    tool_admission: Option<ToolAdmission>,
    coordinator_plan_tools: bool,
    initial_context_summaries: BTreeMap<String, String>,
    skill_catalog: Option<SkillCatalog>,
    project_rules: Option<ProjectRules>,
    task_anchor: Option<TaskAnchor>,
    compacted_checkpoint: Option<CompactedCheckpoint>,
    memory_activation_source: Arc<dyn MemoryActivationSource>,
    allow_bridge_tools: bool,
    allow_low_risk_apply_patches: bool,
    low_risk_process_runner: Option<Arc<dyn ProcessRunner>>,
    read_only_shell_process_runner: Option<Arc<dyn ProcessRunner>>,
    accepted_local_workspace_process_runner: Option<AcceptedLocalWorkspaceProcessRunner>,
    runtime_trust_level: RuntimeTrustLevel,
    permission_review_mode: PermissionReviewMode,
    permission_review_rules: PermissionReviewRules,
    permission_review_workspace_root: Option<PathBuf>,
    permission_actor: crate::PermissionActor,
    permission_admission_source: Option<Arc<dyn PermissionAdmissionSource>>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    background_process_control: Option<Arc<dyn BackgroundProcessControl>>,
    subagent_manager: Option<SubagentManager>,
    subagent_parent_scope: Option<ChildWorkspaceScope>,
    activity_hub: Arc<SubagentActivityHub>,
    session_store: Option<FileSessionStore>,
    loaded_session: Option<SessionState>,
    durable_memory: Option<Arc<crate::DurableMemory>>,
    memory_maintenance: bool,
}

impl RuntimeBuilder {
    pub(super) fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            prompt_cache_key: None,
            event_buffer_size: NonZeroUsize::new(DEFAULT_EVENT_BUFFER_SIZE)
                .expect("default event buffer size is non-zero"),
            max_parallel_tool_calls: NonZeroUsize::new(DEFAULT_MAX_PARALLEL_TOOL_CALLS)
                .expect("default parallel tool-call limit is non-zero"),
            model_configs: RuntimeModelConfigs::default(),
            model_retry_policy: ModelRetryPolicy::default(),
            automatic_compaction: CompactionConfig::default(),
            capabilities: RuntimeCapabilities::default(),
            prompt_profile: PromptProfile::default(),
            progress_commentary: false,
            registered_tools: Vec::new(),
            tool_admission: None,
            coordinator_plan_tools: false,
            initial_context_summaries: BTreeMap::new(),
            skill_catalog: None,
            project_rules: None,
            task_anchor: None,
            compacted_checkpoint: None,
            memory_activation_source: Arc::new(StoredMemoryActivationSource),
            allow_bridge_tools: false,
            allow_low_risk_apply_patches: false,
            low_risk_process_runner: None,
            read_only_shell_process_runner: None,
            accepted_local_workspace_process_runner: None,
            runtime_trust_level: RuntimeTrustLevel::Agent,
            permission_review_mode: PermissionReviewMode::DefaultForTrust,
            permission_review_rules: PermissionReviewRules::default(),
            permission_review_workspace_root: None,
            permission_actor: crate::PermissionActor::MainAgent,
            permission_admission_source: None,
            permissioned_process_runner_factory: None,
            background_process_control: None,
            subagent_manager: None,
            subagent_parent_scope: None,
            activity_hub: Arc::new(SubagentActivityHub::new()),
            session_store: None,
            loaded_session: None,
            durable_memory: None,
            memory_maintenance: false,
        }
    }

    /// Sets the actor identity attached to outgoing host permission review requests.
    #[must_use]
    pub fn permission_actor(mut self, actor: crate::PermissionActor) -> Self {
        self.permission_actor = actor;
        self
    }

    /// Sets the bounded event channel buffer size.
    ///
    /// Runtime event production uses a bounded channel. Capacity counts
    /// internal emission batches rather than individual public events, so one
    /// slot may carry consecutive events whose state must be committed and
    /// enqueued together. Backpressure is part of the state-before-event
    /// contract: producers reserve a batch slot before mutating durable session
    /// state for the corresponding events.
    #[must_use]
    pub fn event_buffer_size(mut self, event_buffer_size: NonZeroUsize) -> Self {
        self.event_buffer_size = event_buffer_size;
        self
    }

    /// Sets the maximum number of adjacent parallel-safe calls executed together.
    #[must_use]
    pub fn max_parallel_tool_calls(mut self, limit: NonZeroUsize) -> Self {
        self.max_parallel_tool_calls = limit;
        self
    }

    /// Overrides the deterministic provider prompt-cache key used for model
    /// requests in this runtime.
    ///
    /// When omitted, model requests default to this runtime's own `session_id`.
    /// Child runtimes use this to share a deterministic cache partition across
    /// sibling subagents running the same role under one parent session.
    #[must_use]
    pub fn prompt_cache_key(mut self, prompt_cache_key: SessionId) -> Self {
        self.prompt_cache_key = Some(prompt_cache_key);
        self
    }

    /// Sets the provider and model used by runtime steps.
    ///
    /// The provider receives normalized model requests and returns normalized
    /// model events from `merry-llm`. Provider response formats are not stored
    /// in runtime state.
    #[must_use]
    pub fn model_provider(mut self, provider: Arc<dyn ModelProvider>, model: ModelName) -> Self {
        self.model_configs.insert(
            RuntimeModelRole::Primary,
            provider,
            model,
            self.model_retry_policy,
        );
        self
    }

    /// Sets the provider and model for a runtime model role.
    ///
    /// Only [`RuntimeModelRole::Primary`] is used by normal runtime steps today.
    /// Non-primary roles are stored as runtime-owned configuration for future
    /// review gates and do not alter provider request compilation.
    #[must_use]
    pub fn model_provider_for_role(
        mut self,
        role: RuntimeModelRole,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
    ) -> Self {
        self.model_configs
            .insert(role, provider, model, self.model_retry_policy);
        self
    }

    /// Sets the provider, model, and retry policy for a runtime model role.
    ///
    /// This is useful for higher-level construction layers that treat provider
    /// config as a component. Unlike [`RuntimeBuilder::model_retry_policy`],
    /// this method does not alter already configured providers.
    #[must_use]
    pub fn model_provider_for_role_with_retry(
        mut self,
        role: RuntimeModelRole,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        retry_policy: ModelRetryPolicy,
    ) -> Self {
        self.model_configs
            .insert(role, provider, model, retry_policy);
        self
    }

    /// Sets the retry policy applied to configured and subsequently configured
    /// model providers.
    #[must_use]
    pub fn model_retry_policy(mut self, policy: ModelRetryPolicy) -> Self {
        self.model_retry_policy = policy;
        self.model_configs.set_retry_policy(policy);
        self
    }

    /// Sets the runtime policy for automatic compaction.
    ///
    /// Automatic compaction runs only when a compiled provider request crosses
    /// the compaction threshold. The current step input is still outside the
    /// compaction input and is projected raw after any installed checkpoint.
    #[must_use]
    pub fn automatic_compaction(mut self, config: CompactionConfig) -> Self {
        self.automatic_compaction = config;
        self
    }

    /// Registers a runtime-owned tool executor.
    ///
    /// Registering a tool makes its spec available to provider requests and
    /// lets [`Runtime::execute_tool_call`] resolve matching pending calls. It
    /// does not start an automatic tool loop.
    #[must_use]
    pub fn register_tool(mut self, tool: RegisteredTool) -> Self {
        self.registered_tools.push(tool);
        self
    }

    /// Installs runtime admission for tool execution without changing the
    /// provider-visible tool catalog.
    #[must_use]
    pub fn tool_admission(mut self, admission: ToolAdmission) -> Self {
        self.tool_admission = Some(admission);
        self
    }

    /// Installs the stable coordinator Plan Mode and Todo tool surface.
    #[must_use]
    pub fn coordinator_plan_tools(mut self) -> Self {
        self.coordinator_plan_tools = true;
        self
    }

    /// Applies a complete runtime profile while keeping this builder as the
    /// construction owner.
    pub fn with_profile(mut self, profile: RuntimeProfile) -> Result<Self, RuntimeError> {
        let parts = profile.into_parts();
        self.capabilities = parts.capabilities;
        self.prompt_profile = parts.prompt_profile;
        if let Some(policy) = parts.model_retry_policy {
            self = self.model_retry_policy(policy);
        }
        self.progress_commentary = parts.progress_commentary;
        for (id, text) in parts.initial_context_summaries {
            self = self.initial_context_summary(&id, &text);
        }
        for tool in parts.registered_tools {
            self = self.register_tool(tool);
        }
        if parts.allow_bridge_tools {
            self = self.allow_bridge_tools();
        }
        if parts.allow_low_risk_apply_patches {
            self = self.allow_low_risk_apply_patches();
        }
        if let Some(runner) = parts.low_risk_process_runner {
            self = self.allow_low_risk_process_actions(runner);
        }
        if let Some(runner) = parts.read_only_shell_process_runner {
            self = self.allow_read_only_shell_process_actions(runner);
        }
        if let Some(accepted) = parts.accepted_local_workspace_process_runner {
            self = self.allow_accepted_local_workspace_process_actions(
                accepted.admission(),
                accepted.runner(),
            );
        }
        if let Some(trust_level) = parts.runtime_trust_level {
            self = self.runtime_trust_level(trust_level);
        }
        if let Some(mode) = parts.permission_review_mode {
            self = self.permission_review_mode(mode);
        }
        if let Some(source) = parts.permission_admission_source {
            self = self.permission_admission_source(source);
        }
        if let Some(factory) = parts.permissioned_process_runner_factory {
            self = self.permissioned_process_runner_factory(factory);
        }
        if let Some(control) = parts.background_process_control {
            self = self.background_process_control(control);
        }
        if let Some(catalog) = parts.skill_catalog {
            self = self.skill_catalog(catalog);
        }
        if let Some(project_rules) = parts.project_rules {
            self = self.project_rules(project_rules);
        }
        if let Some(task_anchor) = parts.task_anchor {
            self = self.task_anchor(task_anchor);
        }
        if let Some(manager) = parts.subagent_manager {
            self = self.subagent_manager(manager);
        }
        Ok(self)
    }

    /// Allows bridge tools to be registered for this runtime.
    ///
    /// Bridge handlers run in host code outside Merry-managed sandboxing. This
    /// opt-in is separate from [`RuntimeCapabilities`] policy.
    #[must_use]
    pub fn allow_bridge_tools(mut self) -> Self {
        self.allow_bridge_tools = true;
        self
    }

    /// Reconciles deterministic runtime context without emitting observable events.
    ///
    /// This is for startup-owned facts such as a compact project capability
    /// summary. The supplied id becomes the current construction-owned seed on
    /// both new and resumed sessions; construction seeds omitted from this
    /// builder are left untouched. It is not a substitute for runtime artifacts
    /// produced during a step, and repeated builder ids are replaced before
    /// build-time validation.
    #[must_use]
    pub fn initial_context_summary(mut self, id: &str, text: &str) -> Self {
        self.initial_context_summaries
            .insert(id.to_owned(), text.to_owned());
        self
    }

    /// Adds explicit project rules to the cacheable stable request prefix.
    ///
    /// This is a construction-time projection for durable project instructions
    /// such as `AGENTS.md`. It does not scan the filesystem and is separate
    /// from context summaries, ledger facts, and artifact payloads.
    #[must_use]
    pub fn project_rules(mut self, project_rules: ProjectRules) -> Self {
        self.project_rules = Some(project_rules);
        self
    }

    /// Adds available skill metadata to the cacheable stable request prefix.
    ///
    /// This projects only `SKILL.md` frontmatter metadata. Full skill bodies
    /// remain on disk and must be read through registered workspace file tools.
    #[must_use]
    pub fn skill_catalog(mut self, skill_catalog: SkillCatalog) -> Self {
        self.skill_catalog = Some(skill_catalog);
        self
    }

    /// Sets the current task objective control-plane anchor.
    ///
    /// This reserves the runtime context slot for future `/task` commands. It
    /// is rendered as dynamic request context, not as project rules, ledger
    /// projection, or ordered transcript history.
    #[must_use]
    pub fn task_anchor(mut self, task_anchor: TaskAnchor) -> Self {
        self.task_anchor = Some(task_anchor);
        self
    }

    /// Sets compacted checkpoint context for dynamic provider requests.
    ///
    /// This is selected by a checkpoint/compaction boundary. It does not
    /// project ordinary ledger facts, artifact payloads, or tool-result
    /// observations. The checkpoint is prose. Resume a real runtime checkpoint
    /// with [`RuntimeBuilder::resume_from_store`] so its transcript and archived
    /// tool refs are restored together.
    #[must_use]
    pub fn compacted_checkpoint(mut self, checkpoint: CompactedCheckpoint) -> Self {
        self.compacted_checkpoint = Some(checkpoint);
        self
    }

    /// Opts in to executing validated low-risk workspace patch proposals.
    ///
    /// This keeps the default policy conservative: workspace writes remain
    /// denied unless the tool provides valid workspace patch proposal evidence
    /// and runtime construction explicitly enables this lane.
    #[must_use]
    pub fn allow_low_risk_apply_patches(mut self) -> Self {
        self.allow_low_risk_apply_patches = true;
        self
    }

    /// Opts in to executing validated low-risk process action proposals.
    ///
    /// The default policy remains deny. This lane is available only for command
    /// execution proposals with provider-neutral process evidence, an injected
    /// runtime runner, and the narrow SP2 low-risk predicate.
    #[must_use]
    pub fn allow_low_risk_process_actions(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.low_risk_process_runner = Some(runner);
        self
    }

    /// Opts in to executing validated read-only shell wrapper proposals.
    ///
    /// This lane is intentionally separate from the structured low-risk argv
    /// lane. It accepts only a narrow `bash`/`sh`/`zsh -c|-lc` plain command
    /// sequence classifier and requires an injected runner selected for the
    /// shell read-only profile. It does not authorize arbitrary shell syntax.
    #[must_use]
    pub fn allow_read_only_shell_process_actions(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.read_only_shell_process_runner = Some(runner);
        self
    }

    /// Opts in to executing validated local workspace effect process proposals.
    ///
    /// Runner injection alone is not a sandbox or an authorization source. This
    /// lane requires explicit runtime construction-time admission that declares
    /// the sandbox profile and accepted local workspace process risk for the
    /// narrow classified process intent.
    #[must_use]
    pub fn allow_accepted_local_workspace_process_actions(
        mut self,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
    ) -> Self {
        self.accepted_local_workspace_process_runner =
            Some(AcceptedLocalWorkspaceProcessRunner { admission, runner });
        self
    }

    /// Sets the trust level used by permission request admission defaults.
    ///
    /// Agentic/coding-agent runtimes require review by default. Trusted SDK
    /// hosts may explicitly choose host-only admission for business workflows
    /// where the host already owns the action policy.
    #[must_use]
    pub fn runtime_trust_level(mut self, trust_level: RuntimeTrustLevel) -> Self {
        self.runtime_trust_level = trust_level;
        self
    }

    /// Sets how permission requests are reviewed before execution.
    #[must_use]
    pub fn permission_review_mode(mut self, mode: PermissionReviewMode) -> Self {
        self.permission_review_mode = mode;
        self
    }

    /// Sets user-trusted custom rules for AI permission review.
    #[must_use]
    pub fn permission_review_rules(mut self, rules: PermissionReviewRules) -> Self {
        self.permission_review_rules = rules;
        self
    }

    /// Sets the workspace root used for read-only harness environment probes
    /// (such as `git status` cleanliness) during AI permission review.
    #[must_use]
    pub fn permission_review_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.permission_review_workspace_root = Some(root.into());
        self
    }

    /// Explicitly enables fully trusted SDK/host execution for registered
    /// tools. Runtime schema validation, tool executor boundaries, sandboxing,
    /// and cancellation remain active. Proposal-based commit lifecycle checks
    /// are intentionally bypassed at this explicit trust boundary.
    #[must_use]
    pub fn fully_trusted_tools(self) -> Self {
        self.permission_review_mode(PermissionReviewMode::FullyTrusted)
    }

    /// Installs a host-owned permission admission source.
    ///
    /// This is used for trusted SDK hosts or tests. Agentic runtimes should
    /// normally use the model-backed review selected from
    /// [`RuntimeModelRole::ApprovalReview`] with primary fallback.
    #[must_use]
    pub fn permission_admission_source(
        mut self,
        source: Arc<dyn PermissionAdmissionSource>,
    ) -> Self {
        self.permission_admission_source = Some(source);
        self
    }

    /// Opts in to executing process actions approved by `request_permissions`.
    ///
    /// The runner should represent the backend/profile used for approved
    /// permission requests. Runtime admission approves only the exact planned
    /// action, while a concrete backend may retain the approved capabilities
    /// for later actions in the same runtime session.
    #[must_use]
    pub fn allow_permissioned_process_actions(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.permissioned_process_runner_factory = Some(Arc::new(
            StaticPermissionedProcessRunnerFactory::new(runner),
        ));
        self
    }

    /// Opts in to constructing a process runner per approved permission request.
    ///
    /// This is the preferred path when approved capabilities must be
    /// materialized into each new action while remaining active for the
    /// current runtime session.
    #[must_use]
    pub fn permissioned_process_runner_factory(
        mut self,
        factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        self.permissioned_process_runner_factory = Some(factory);
        self
    }

    /// Installs the board that lists, reads, and stops background processes.
    #[must_use]
    pub fn background_process_control(
        mut self,
        control: Arc<dyn BackgroundProcessControl>,
    ) -> Self {
        self.background_process_control = Some(control);
        self
    }

    /// Installs a runtime-owned subagent manager for future subagent tools.
    #[must_use]
    pub fn subagent_manager(mut self, manager: SubagentManager) -> Self {
        self.subagent_manager = Some(manager);
        self
    }

    /// Reuses a runtime-owned activity hub for this runtime and its children.
    #[must_use]
    pub fn subagent_activity_hub(mut self, hub: Arc<SubagentActivityHub>) -> Self {
        self.activity_hub = hub;
        self
    }

    /// Sets the effective workspace scope inherited by nested subagent tasks.
    #[must_use]
    pub fn subagent_parent_scope(mut self, scope: ChildWorkspaceScope) -> Self {
        self.subagent_parent_scope = Some(scope);
        self
    }

    /// Installs the filesystem store used by explicit and automatic session savepoints.
    #[must_use]
    pub fn session_store(mut self, store: FileSessionStore) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Installs a session that was already loaded by the runtime boundary.
    #[must_use]
    pub fn with_loaded_session(mut self, loaded: LoadedSession) -> Self {
        self.loaded_session = Some(loaded.into_state());
        self
    }

    /// Loads persisted session state from an injected filesystem store, then builds a runtime.
    pub async fn resume_from_store(
        mut self,
        store: FileSessionStore,
    ) -> Result<Runtime, RuntimeError> {
        let session = SessionState::load_from(&store, &self.session_id).await?;
        self.session_store = Some(store);
        self.loaded_session = Some(session);
        let runtime = self.build()?;
        Ok(runtime)
    }

    /// Loads persisted state without replaying or restarting Plan execution.
    pub async fn resume_from_store_without_automatic_savepoints(
        mut self,
        store: FileSessionStore,
    ) -> Result<Runtime, RuntimeError> {
        self.loaded_session = Some(SessionState::load_from(&store, &self.session_id).await?);
        let runtime = self.build()?;
        Ok(runtime)
    }

    /// Loads persisted session state without enabling automatic savepoints.
    pub async fn load_session_from_store(
        mut self,
        store: FileSessionStore,
    ) -> Result<Self, RuntimeError> {
        self.loaded_session = Some(SessionState::load_from(&store, &self.session_id).await?);
        Ok(self)
    }

    /// Attaches durable memory and freezes its index for this runtime.
    ///
    /// `maintain` schedules capture and one-shot dream after a completed parent
    /// turn. Child runtimes keep the same store with maintenance disabled.
    #[must_use]
    pub fn durable_memory(mut self, memory: Arc<crate::DurableMemory>, maintain: bool) -> Self {
        self.durable_memory = Some(memory);
        self.memory_maintenance = maintain;
        self
    }

    /// Builds the runtime.
    ///
    /// Duplicate tool names are rejected before the runtime is constructed.
    pub fn build(self) -> Result<Runtime, RuntimeError> {
        let mut registered_tools = self.registered_tools;
        if self.coordinator_plan_tools {
            registered_tools
                .extend(coordinator_plan_registered_tools().map_err(RuntimeError::from)?);
            registered_tools.push(todo_registered_tool().map_err(RuntimeError::from)?);
        }
        let tool_registry =
            ToolRegistry::from_registered(registered_tools).map_err(|error| match error {
                ToolRegistryError::DuplicateName { name } => {
                    RuntimeError::DuplicateToolRegistration { name }
                }
                ToolRegistryError::ReservedName { name } => RuntimeError::ReservedToolName { name },
                ToolRegistryError::InvalidToolInputSchema { name, message } => {
                    RuntimeError::InvalidToolInputSchema { name, message }
                }
            })?;
        if !self.allow_bridge_tools
            && let Some(name) = tool_registry.first_bridge_tool_name()
        {
            return Err(RuntimeError::BridgeToolsNotAllowed { name: name.clone() });
        }

        let effective_subagent_scope = self.subagent_manager.as_ref().map(|_| {
            self.subagent_parent_scope
                .clone()
                .unwrap_or_else(ChildWorkspaceScope::workspace_root)
        });

        let external_tool_catalog = tool_registry.external_tool_catalog()?;
        let mut session = match self.loaded_session {
            Some(mut session) => {
                session.set_external_tool_catalog(external_tool_catalog);
                session
            }
            None => {
                let mut session = SessionState::with_external_tool_catalog(
                    self.session_id.clone(),
                    external_tool_catalog,
                );
                if let Some(checkpoint) = self.compacted_checkpoint {
                    session.set_compacted_checkpoint(checkpoint);
                }
                session
            }
        };
        if let Some(store) = self.session_store.as_ref() {
            session.set_tracked_store_dir(store.sessions_dir().to_path_buf());
        }
        if session.session_id() != &self.session_id {
            return Err(RuntimeError::SessionStore {
                source: crate::SessionStoreError::SessionIdMismatch {
                    requested: self.session_id.clone(),
                    actual: session.session_id().clone(),
                },
            });
        }
        // Scope is runtime/session-specific dynamic context; keep it out of
        // the stable provider prefix and out of the user task text.
        if let Some(scope) = effective_subagent_scope.as_ref() {
            let scope_text = scope.prompt_declaration();
            session.reconcile_construction_context_seed(
                SUBAGENT_WORKSPACE_SCOPE_CONTEXT_ID,
                &scope_text,
            )?;
        }
        for (id, text) in self.initial_context_summaries {
            session.reconcile_construction_context_seed(&id, &text)?;
        }
        let trajectory =
            RuntimeObservability::new(self.session_id.clone(), tool_registry.tool_specs());
        trajectory.seed_prompt_profile(
            &self.prompt_profile,
            self.progress_commentary,
            self.skill_catalog.as_ref(),
            self.project_rules.as_ref(),
        );
        if let Some(project_rules) = self.project_rules {
            session.set_project_rules(project_rules);
        }
        if let Some(skill_catalog) = self.skill_catalog {
            session.set_skill_catalog(skill_catalog);
        }
        if let Some(task_anchor) = self.task_anchor {
            session.set_task_anchor(task_anchor);
        }

        trajectory.seed_from_session(&session);

        let memory_context = match &self.durable_memory {
            Some(memory) => match memory.render_context() {
                Ok(text) => Some(Arc::<str>::from(text)),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "durable memory context was not injected"
                    );
                    None
                }
            },
            None => None,
        };
        let persisted_subagents = session.persisted_subagents().to_vec();
        let session = Arc::new(Mutex::new(session));
        let subagent_event_sink = crate::subagent::SubagentEventSink::new(
            Arc::clone(&session),
            self.session_store.clone(),
            Arc::clone(&trajectory),
            self.event_buffer_size.get().max(64),
        );
        let activity_hub = self.activity_hub;
        let subagent_manager = self.subagent_manager;
        if let Some(manager) = subagent_manager.as_ref() {
            let allowed_tools = tool_registry
                .tool_specs()
                .into_iter()
                .map(|spec| spec.name().clone())
                .collect::<Vec<ToolName>>();
            let parent_scope = effective_subagent_scope
                .as_ref()
                .expect("effective scope exists when a subagent manager is present")
                .clone();
            manager.attach_parent_capabilities(allowed_tools, parent_scope);
            manager.attach_activity_hub(Arc::clone(&activity_hub));
            manager.attach_event_sink(subagent_event_sink.clone());
            if let Some(store) = self.session_store.as_ref() {
                manager.attach_child_session_store(store.subagents_store(&self.session_id));
            }
            if !persisted_subagents.is_empty() {
                manager.hydrate_persisted(persisted_subagents);
            }
            if let Some(primary) = self.model_configs.get(RuntimeModelRole::Primary) {
                manager.update_parent_primary_model(crate::ParentPrimaryModel {
                    provider: primary.provider(),
                    model: primary.model().clone(),
                    retry_policy: primary.retry_policy(),
                });
            }
        }
        Ok(Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: self.session_id.clone(),
                prompt_cache_key: self
                    .prompt_cache_key
                    .unwrap_or_else(|| self.session_id.clone()),
                session,
                active_step: Arc::new(AtomicBool::new(false)),
                memory_projection_epoch: AtomicU64::new(0),
                event_buffer_size: self.event_buffer_size,
                max_parallel_tool_calls: self.max_parallel_tool_calls,
                model_configs: self.model_configs,
                primary_model_override: RwLock::new(None),
                automatic_compaction: RwLock::new(self.automatic_compaction),
                context_window_tokens: RwLock::new(None),
                capabilities: self.capabilities,
                prompt_profile: self.prompt_profile,
                progress_commentary: self.progress_commentary,
                tool_registry,
                tool_admission: self.tool_admission,
                memory_activation_source: self.memory_activation_source,
                allow_low_risk_apply_patches: self.allow_low_risk_apply_patches,
                low_risk_process_runner: self.low_risk_process_runner,
                read_only_shell_process_runner: self.read_only_shell_process_runner,
                accepted_local_workspace_process_runner: self
                    .accepted_local_workspace_process_runner,
                runtime_trust_level: self.runtime_trust_level,
                permission_review_mode: self.permission_review_mode,
                permission_review_rules: self.permission_review_rules,
                permission_review_workspace_root: self.permission_review_workspace_root,
                permission_actor: self.permission_actor,
                recorded_permission_decisions: RwLock::new(VecDeque::new()),
                permission_admission_source: self.permission_admission_source,
                permissioned_process_runner_factory: self.permissioned_process_runner_factory,
                background_process_control: self.background_process_control,
                subagent_manager,
                coordinator_plan_tools: self.coordinator_plan_tools,
                session_store: self.session_store,
                tool_batch_active: AtomicBool::new(false),
                activity_hub,
                subagent_event_sink,
                trajectory,
                durable_memory: self.durable_memory,
                memory_maintenance: self.memory_maintenance,
                memory_context,
            }),
        })
    }
}
