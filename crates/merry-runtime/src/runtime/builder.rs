use super::auto_compaction::default_automatic_compaction_policy;
use super::checkpoint_ref_tool::merry_read_checkpoint_ref_tool;
use super::{AcceptedLocalWorkspaceProcessRunner, Runtime, RuntimeInner};
use crate::{
    AcceptedLocalWorkspaceProcessAdmission, CitationCompactionPolicy, CompactedCheckpoint,
    FileSessionStore, ProcessRunner, ProjectRules, RuntimeCapabilities, RuntimeError,
    RuntimeModelRole, RuntimeProfile, SkillCatalog, TaskAnchor,
    artifact::ArtifactContent,
    memory::{MemoryActivationSource, StoredMemoryActivationSource},
    model_config::RuntimeModelConfigs,
    permission::{PermissionAdmissionSource, PermissionReviewMode, RuntimeTrustLevel},
    plan::{
        PlanController, PlanScheduler, PlanWorkerControl,
        tools::{coordinator_plan_registered_tools, worker_plan_registered_tools},
    },
    process::{PermissionedProcessRunnerFactory, StaticPermissionedProcessRunnerFactory},
    session::SessionState,
    subagent::{ChildRuntimeFactory, SubagentManager},
    tool::{RegisteredTool, ToolRegistry, ToolRegistryError},
};
use merry_core::{ArtifactRef, SessionId};
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
};
use tokio::sync::{Mutex, RwLock};

const DEFAULT_EVENT_BUFFER_SIZE: usize = 16;

/// Runtime-owned policy for automatic checkpoint compaction.
///
/// This controls the pre-provider hard-watermark compaction path. Manual
/// [`Runtime::compact_context_once`] calls still take an explicit
/// [`CitationCompactionPolicy`] so tests and callers can run one-off compaction
/// passes without mutating runtime construction policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomaticCompactionConfig {
    enabled: bool,
    policy: CitationCompactionPolicy,
}

impl AutomaticCompactionConfig {
    /// Enables automatic hard-watermark compaction with the provided policy.
    #[must_use]
    pub fn enabled(policy: CitationCompactionPolicy) -> Self {
        Self {
            enabled: true,
            policy,
        }
    }

    /// Disables automatic hard-watermark compaction.
    ///
    /// The policy remains populated with defaults so disabled configs can be
    /// inspected or re-enabled by callers without constructing a dummy policy.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            policy: default_automatic_compaction_policy(),
        }
    }

    #[must_use]
    pub fn is_enabled(self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn policy(self) -> CitationCompactionPolicy {
        self.policy
    }
}

impl Default for AutomaticCompactionConfig {
    fn default() -> Self {
        Self::enabled(default_automatic_compaction_policy())
    }
}

/// Builder for a Merry runtime.
///
/// The builder wires provider-neutral runtime configuration: event buffering,
/// one optional model provider, and zero or more runtime-owned tool executors.
/// Provider integrations stay outside this crate behind [`ModelProvider`].
pub struct RuntimeBuilder {
    session_id: SessionId,
    event_buffer_size: NonZeroUsize,
    max_parallel_tool_calls: NonZeroUsize,
    model_configs: RuntimeModelConfigs,
    model_retry_policy: ModelRetryPolicy,
    automatic_compaction: AutomaticCompactionConfig,
    capabilities: RuntimeCapabilities,
    progress_commentary: bool,
    registered_tools: Vec<RegisteredTool>,
    coordinator_plan_tools: bool,
    worker_plan_control: Option<PlanWorkerControl>,
    initial_context_summaries: BTreeMap<String, String>,
    skill_catalog: Option<SkillCatalog>,
    project_rules: Option<ProjectRules>,
    task_anchor: Option<TaskAnchor>,
    compacted_checkpoint: Option<CompactedCheckpoint>,
    compacted_checkpoint_evidence: Vec<(ArtifactRef, ArtifactContent)>,
    memory_activation_source: Arc<dyn MemoryActivationSource>,
    allow_bridge_tools: bool,
    allow_low_risk_workspace_patches: bool,
    low_risk_process_runner: Option<Arc<dyn ProcessRunner>>,
    read_only_shell_process_runner: Option<Arc<dyn ProcessRunner>>,
    accepted_local_workspace_process_runner: Option<AcceptedLocalWorkspaceProcessRunner>,
    runtime_trust_level: RuntimeTrustLevel,
    permission_review_mode: PermissionReviewMode,
    permission_admission_source: Option<Arc<dyn PermissionAdmissionSource>>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    subagent_manager: Option<SubagentManager>,
    plan_worker_factory: Option<Arc<dyn ChildRuntimeFactory>>,
    plan_worker_max_threads: usize,
    session_store: Option<FileSessionStore>,
    loaded_session: Option<SessionState>,
}

impl RuntimeBuilder {
    pub(super) fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            event_buffer_size: NonZeroUsize::new(DEFAULT_EVENT_BUFFER_SIZE)
                .expect("default event buffer size is non-zero"),
            max_parallel_tool_calls: NonZeroUsize::new(4)
                .expect("default parallel tool-call limit is non-zero"),
            model_configs: RuntimeModelConfigs::default(),
            model_retry_policy: ModelRetryPolicy::default(),
            automatic_compaction: AutomaticCompactionConfig::default(),
            capabilities: RuntimeCapabilities::default(),
            progress_commentary: false,
            registered_tools: Vec::new(),
            coordinator_plan_tools: false,
            worker_plan_control: None,
            initial_context_summaries: BTreeMap::new(),
            skill_catalog: None,
            project_rules: None,
            task_anchor: None,
            compacted_checkpoint: None,
            compacted_checkpoint_evidence: Vec::new(),
            memory_activation_source: Arc::new(StoredMemoryActivationSource),
            allow_bridge_tools: false,
            allow_low_risk_workspace_patches: false,
            low_risk_process_runner: None,
            read_only_shell_process_runner: None,
            accepted_local_workspace_process_runner: None,
            runtime_trust_level: RuntimeTrustLevel::Agent,
            permission_review_mode: PermissionReviewMode::DefaultForTrust,
            permission_admission_source: None,
            permissioned_process_runner_factory: None,
            subagent_manager: None,
            plan_worker_factory: None,
            plan_worker_max_threads: 1,
            session_store: None,
            loaded_session: None,
        }
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

    /// Sets the runtime policy for automatic hard-watermark compaction.
    ///
    /// Automatic compaction runs only when a compiled provider request crosses
    /// the hard context watermark. The current step input is still outside the
    /// compaction input and is projected raw after any installed checkpoint.
    #[must_use]
    pub fn automatic_compaction(mut self, config: AutomaticCompactionConfig) -> Self {
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

    /// Installs the complete stable coordinator plan-control tool surface.
    #[must_use]
    pub fn coordinator_plan_tools(mut self) -> Self {
        self.coordinator_plan_tools = true;
        self
    }

    /// Installs the stable worker-scoped plan reporting surface.
    #[must_use]
    pub fn plan_worker_control(mut self, control: PlanWorkerControl) -> Self {
        self.worker_plan_control = Some(control);
        self
    }

    /// Installs the depth-one child factory used by the central plan scheduler.
    #[must_use]
    pub fn plan_worker_factory(
        mut self,
        factory: Arc<dyn ChildRuntimeFactory>,
        max_worker_threads: usize,
    ) -> Self {
        self.plan_worker_factory = Some(factory);
        self.plan_worker_max_threads = max_worker_threads.max(1);
        self
    }

    /// Applies a complete runtime profile while keeping this builder as the
    /// construction owner.
    pub fn with_profile(mut self, profile: RuntimeProfile) -> Result<Self, RuntimeError> {
        let parts = profile.into_parts();
        self.capabilities = parts.capabilities;
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
        if parts.allow_low_risk_workspace_patches {
            self = self.allow_low_risk_workspace_patches();
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
    /// observations. Citation-backed checkpoints also require every referenced
    /// artifact to be supplied with [`RuntimeBuilder::compacted_checkpoint_evidence`].
    ///
    /// Manual checkpoint construction is intended for deterministic fixtures or
    /// fresh bootstrap state without transcript history. Resume a real runtime
    /// checkpoint with [`RuntimeBuilder::resume_from_store`] so its transcript
    /// artifacts and ids are restored together. Manual refs matching `h<decimal>`
    /// are rejected because that namespace belongs to runtime transcript history;
    /// use a distinct prefix such as `bootstrap-` for fresh bootstrap refs.
    #[must_use]
    pub fn compacted_checkpoint(mut self, checkpoint: CompactedCheckpoint) -> Self {
        self.compacted_checkpoint = Some(checkpoint);
        self
    }

    /// Seeds one original artifact used by a manually supplied citation checkpoint.
    ///
    /// This method may be called once per referenced artifact. [`RuntimeBuilder::build`]
    /// records all seeds, validates every checkpoint evidence locator, and only
    /// then installs the checkpoint. Runtime-reserved artifact ids are rejected.
    /// Real transcript-backed checkpoints must be restored with
    /// [`RuntimeBuilder::resume_from_store`] instead of manually seeded here.
    #[must_use]
    pub fn compacted_checkpoint_evidence(
        mut self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Self {
        self.compacted_checkpoint_evidence.push((artifact, content));
        self
    }

    /// Opts in to executing validated low-risk workspace patch proposals.
    ///
    /// This keeps the default policy conservative: workspace writes remain
    /// denied unless the tool provides valid workspace patch proposal evidence
    /// and runtime construction explicitly enables this lane.
    #[must_use]
    pub fn allow_low_risk_workspace_patches(mut self) -> Self {
        self.allow_low_risk_workspace_patches = true;
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
    /// action; it does not grant a reusable id back to the model.
    #[must_use]
    pub fn allow_permissioned_process_actions(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.permissioned_process_runner_factory = Some(Arc::new(
            StaticPermissionedProcessRunnerFactory::new(runner),
        ));
        self
    }

    /// Opts in to constructing a process runner per approved permission request.
    ///
    /// This is the preferred path for sandbox backends such as bubblewrap where
    /// approved capabilities, currently network, should be materialized only
    /// for the exact action being executed.
    #[must_use]
    pub fn permissioned_process_runner_factory(
        mut self,
        factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        self.permissioned_process_runner_factory = Some(factory);
        self
    }

    /// Installs a runtime-owned subagent manager for future subagent tools.
    #[must_use]
    pub fn subagent_manager(mut self, manager: SubagentManager) -> Self {
        self.subagent_manager = Some(manager);
        self
    }

    /// Installs the filesystem store used by explicit and automatic session savepoints.
    #[must_use]
    pub fn session_store(mut self, store: FileSessionStore) -> Self {
        self.session_store = Some(store);
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
        runtime.inner.ensure_plan_scheduler_started();
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

    /// Builds the runtime.
    ///
    /// Duplicate tool names are rejected before the runtime is constructed.
    pub fn build(self) -> Result<Runtime, RuntimeError> {
        let mut registered_tools = self.registered_tools;
        if self.coordinator_plan_tools && self.worker_plan_control.is_some() {
            return Err(RuntimeError::InvalidStepInput {
                reason: "a runtime cannot be both a plan coordinator and a plan worker",
            });
        }
        if self.coordinator_plan_tools {
            registered_tools
                .extend(coordinator_plan_registered_tools().map_err(RuntimeError::from)?);
        }
        if self.worker_plan_control.is_some() {
            registered_tools.extend(worker_plan_registered_tools().map_err(RuntimeError::from)?);
        }
        if self.automatic_compaction.is_enabled() {
            registered_tools.push(merry_read_checkpoint_ref_tool().map_err(RuntimeError::from)?);
        }
        let tool_registry =
            ToolRegistry::from_registered(registered_tools).map_err(|error| match error {
                ToolRegistryError::DuplicateName { name } => {
                    RuntimeError::DuplicateToolRegistration { name }
                }
                ToolRegistryError::InvalidToolInputSchema { name, message } => {
                    RuntimeError::InvalidToolInputSchema { name, message }
                }
            })?;
        if !self.allow_bridge_tools
            && let Some(name) = tool_registry.first_bridge_tool_name()
        {
            return Err(RuntimeError::BridgeToolsNotAllowed { name: name.clone() });
        }

        let mut session = match self.loaded_session {
            Some(session) => session,
            None => {
                if let Some(checkpoint) = self
                    .compacted_checkpoint
                    .as_ref()
                    .and_then(CompactedCheckpoint::citation_backed)
                {
                    for reference in checkpoint.manifest().refs() {
                        if reference.id().is_runtime_history_ref() {
                            return Err(
                                crate::CheckpointError::ManualCheckpointHistoryRefReserved {
                                    ref_id: reference.id().as_str().to_owned(),
                                }
                                .into(),
                            );
                        }
                    }
                }
                let mut session = SessionState::new(self.session_id.clone());
                for (artifact, content) in self.compacted_checkpoint_evidence {
                    if crate::session::is_runtime_reserved_artifact_id(artifact.id()) {
                        return Err(RuntimeError::ReservedArtifactId {
                            artifact_id: artifact.id().clone(),
                        });
                    }
                    session.record_artifact_state(artifact, content)?;
                }
                if let Some(checkpoint) = self.compacted_checkpoint {
                    session.validate_compacted_checkpoint_evidence(&checkpoint)?;
                    session.set_compacted_checkpoint(checkpoint);
                }
                session
            }
        };
        if session.session_id() != &self.session_id {
            return Err(RuntimeError::SessionStore {
                source: crate::SessionStoreError::SessionIdMismatch {
                    requested: self.session_id.clone(),
                    actual: session.session_id().clone(),
                },
            });
        }
        for (id, text) in self.initial_context_summaries {
            session.reconcile_construction_context_seed(&id, &text)?;
        }
        if let Some(project_rules) = self.project_rules {
            session.set_project_rules(project_rules);
        }
        if let Some(skill_catalog) = self.skill_catalog {
            session.set_skill_catalog(skill_catalog);
        }
        if let Some(task_anchor) = self.task_anchor {
            session.set_task_anchor(task_anchor);
        }

        let session = Arc::new(Mutex::new(session));
        let (plan_controller, _plan_events) = PlanController::start(
            Arc::clone(&session),
            self.session_store.clone(),
            self.event_buffer_size,
        );
        let plan_scheduler = self.worker_plan_control.is_none().then(|| {
            PlanScheduler::new(
                plan_controller.clone(),
                self.plan_worker_factory,
                self.plan_worker_max_threads,
                self.session_id.clone(),
            )
        });

        Ok(Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: self.session_id.clone(),
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
                progress_commentary: self.progress_commentary,
                tool_registry,
                memory_activation_source: self.memory_activation_source,
                allow_low_risk_workspace_patches: self.allow_low_risk_workspace_patches,
                low_risk_process_runner: self.low_risk_process_runner,
                read_only_shell_process_runner: self.read_only_shell_process_runner,
                accepted_local_workspace_process_runner: self
                    .accepted_local_workspace_process_runner,
                runtime_trust_level: self.runtime_trust_level,
                permission_review_mode: self.permission_review_mode,
                permission_admission_source: self.permission_admission_source,
                permissioned_process_runner_factory: self.permissioned_process_runner_factory,
                subagent_manager: self.subagent_manager,
                plan_controller,
                worker_plan_control: self.worker_plan_control,
                plan_scheduler,
                session_store: self.session_store,
            }),
        })
    }
}
