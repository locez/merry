//! Parent coding-runtime composition.
//!
//! This module owns the typed assembly boundary between a coding surface and
//! the provider-neutral runtime. It constructs runtime state through
//! [`merry_runtime::RuntimeBuilder`], but it does not own that state after the
//! builder returns a [`CodingRuntime`].

use crate::child_runtime::{CodingChildRuntimeFactory, CodingRuntimeComposition};
use crate::{
    CodingAgentProfile, CodingAgentProfileBuildError, CodingAgentProfileBuilder,
    ProjectRulesLoadError, coding_agent, load_root_project_rules,
};
use merry_core::{CoreError, SessionId};
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use merry_process::ProcessBackend;
use merry_runtime::{
    AgentLoopConfig, AgentLoopConfigError, AutomaticCompactionConfig, ChildRuntimeFactory,
    FileSessionStore, PermissionAdmissionSource, PermissionReviewMode, RegisteredTool, Runtime,
    RuntimeBuilder, RuntimeError, RuntimeModelRole, SkillCatalog, SkillError, SubagentConfig,
    SubagentManager, subagent_registered_tools,
};
use merry_tool_workspace::WorkspaceToolLimits;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};
use thiserror::Error;

/// A provider-neutral model/provider pair for one non-primary runtime role.
///
/// Provider adapters are represented by the Merry-owned [`ModelProvider`] trait;
/// provider wire request and response types never cross this contract.
#[derive(Clone)]
pub struct CodingModelRoleConfig {
    role: RuntimeModelRole,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
}

impl CodingModelRoleConfig {
    /// Creates a role-specific model configuration.
    pub fn new(
        role: RuntimeModelRole,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
    ) -> Result<Self, CodingModelRoleConfigError> {
        if role == RuntimeModelRole::Primary {
            return Err(CodingModelRoleConfigError::PrimaryRole);
        }
        Ok(Self {
            role,
            provider,
            model,
        })
    }

    /// Returns the runtime role receiving this provider/model pair.
    #[must_use]
    pub const fn role(&self) -> RuntimeModelRole {
        self.role
    }

    /// Returns the provider handle for this role.
    #[must_use]
    pub fn provider(&self) -> Arc<dyn ModelProvider> {
        Arc::clone(&self.provider)
    }

    /// Returns the model name for this role.
    #[must_use]
    pub fn model(&self) -> &ModelName {
        &self.model
    }
}

/// Error returned when a secondary coding role is constructed with the
/// primary role identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodingModelRoleConfigError {
    /// The primary provider/model belongs to [`CodingRuntimeInput::new`].
    #[error("primary model must be configured as the coding runtime's primary provider")]
    PrimaryRole,
}

/// Product process boundary used to select coding permission policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingProcessBoundary {
    /// Process actions run directly in the host environment.
    Unrestricted,
    /// Process actions use only the per-action inner sandbox.
    InnerOnly,
    /// Process actions use the outer Merry sandbox and the inner sandbox.
    OuterAndInner,
}

/// Host/model preference for permission review when the outer sandbox is off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NoSandboxReviewMode {
    /// Use the host admission source directly when one is available.
    #[default]
    Host,
    /// Try model review first and fall back to the host when one is available.
    Model,
}

/// Trust mode selected by the product surface for a coding runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodingTrustMode {
    /// Permission requests must go through the configured review policy.
    #[default]
    Reviewed,
    /// Explicitly skip model and host permission review for configured actions.
    FullyTrusted,
}

/// Failure to construct a process-boundary permission policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodingPermissionPolicyError {
    /// A selected policy needs a host admission source, but the surface did not provide one.
    #[error("{boundary:?} permission review requires a host admission source")]
    HostAdmissionUnavailable { boundary: CodingProcessBoundary },
}

/// Permission configuration shared by parent and child coding runtimes.
///
/// This is the only coding-layer representation of permission mode and host
/// admission. Host-dependent variants carry their source directly, so an
/// incomplete host policy cannot be constructed by callers.
#[derive(Clone, Default)]
pub enum CodingPermissionPolicy {
    /// Use the runtime's trust-level default.
    #[default]
    Default,
    /// Route permission requests through model-backed review without a host fallback.
    ///
    /// The variant name is retained for public API compatibility; use
    /// [`CodingPermissionPolicy::model_only`] in new code.
    Required,
    /// Route permission requests through the supplied host admission source.
    HostDecisionOnly {
        /// Host-owned admission source used by the runtime.
        source: Arc<dyn PermissionAdmissionSource>,
    },
    /// Try model-backed review first and use the supplied host source as a fallback.
    ModelThenHostFallback {
        /// Host-owned admission source used by the runtime.
        source: Arc<dyn PermissionAdmissionSource>,
    },
    /// Admit configured registered tools without an approval round.
    FullyTrusted,
}

impl CodingPermissionPolicy {
    /// Route permission requests through model-backed review without a host fallback.
    #[must_use]
    pub const fn model_only() -> Self {
        Self::Required
    }

    /// Route permission requests through model-backed review without a host fallback.
    #[must_use]
    pub const fn required() -> Self {
        Self::model_only()
    }

    /// Route permission requests through the supplied host admission source.
    #[must_use]
    pub fn host_decision_only(source: Arc<dyn PermissionAdmissionSource>) -> Self {
        Self::HostDecisionOnly { source }
    }

    /// Try model-backed review first and use the supplied host source as a fallback.
    #[must_use]
    pub fn model_then_host_fallback(source: Arc<dyn PermissionAdmissionSource>) -> Self {
        Self::ModelThenHostFallback { source }
    }

    /// Admit configured registered tools without an approval round.
    #[must_use]
    pub const fn fully_trusted() -> Self {
        Self::FullyTrusted
    }

    /// Selects the product policy for one process boundary.
    ///
    /// A host fallback is only constructed when the caller supplies a host
    /// admission source. Callers that need host review must handle the typed
    /// error instead of silently degrading to another policy.
    pub fn for_process_boundary(
        boundary: CodingProcessBoundary,
        trust: CodingTrustMode,
        no_sandbox_review: NoSandboxReviewMode,
        host_source: Option<Arc<dyn PermissionAdmissionSource>>,
    ) -> Result<Self, CodingPermissionPolicyError> {
        if trust == CodingTrustMode::FullyTrusted {
            return Ok(Self::fully_trusted());
        }

        match boundary {
            CodingProcessBoundary::OuterAndInner => Ok(Self::model_only()),
            CodingProcessBoundary::InnerOnly => host_source
                .map(Self::model_then_host_fallback)
                .ok_or(CodingPermissionPolicyError::HostAdmissionUnavailable { boundary }),
            CodingProcessBoundary::Unrestricted => match no_sandbox_review {
                NoSandboxReviewMode::Host => host_source
                    .map(Self::host_decision_only)
                    .ok_or(CodingPermissionPolicyError::HostAdmissionUnavailable { boundary }),
                NoSandboxReviewMode::Model => host_source
                    .map(Self::model_then_host_fallback)
                    .ok_or(CodingPermissionPolicyError::HostAdmissionUnavailable { boundary }),
            },
        }
    }

    fn apply_to(&self, mut builder: RuntimeBuilder) -> RuntimeBuilder {
        match self {
            Self::Default => {}
            Self::Required => {
                builder = builder.permission_review_mode(PermissionReviewMode::Required);
            }
            Self::HostDecisionOnly { source } => {
                builder = builder
                    .permission_review_mode(PermissionReviewMode::HostDecisionOnly)
                    .permission_admission_source(Arc::clone(source));
            }
            Self::ModelThenHostFallback { source } => {
                builder = builder
                    .permission_review_mode(PermissionReviewMode::ModelThenHostFallback)
                    .permission_admission_source(Arc::clone(source));
            }
            Self::FullyTrusted => {
                builder = builder.permission_review_mode(PermissionReviewMode::FullyTrusted);
            }
        }
        builder
    }
}

/// Parent/child policy that must be applied to every coding runtime builder.
///
/// The policy contains only validated provider-neutral inputs. Runtime owns the
/// meaning of model roles and permission admission; this type only forwards the
/// same values to the parent and child builders.
#[derive(Clone, Default)]
pub(crate) struct CodingRuntimePolicy {
    model_roles: Vec<CodingModelRoleConfig>,
    permission: CodingPermissionPolicy,
}

impl CodingRuntimePolicy {
    pub(crate) fn try_new(
        model_roles: Vec<CodingModelRoleConfig>,
        permission: CodingPermissionPolicy,
    ) -> Result<Self, CodingRuntimeBuildError> {
        let mut configured_roles = BTreeSet::new();
        for role in &model_roles {
            if !configured_roles.insert(role.role()) {
                return Err(CodingRuntimeBuildError::DuplicateModelRole { role: role.role() });
            }
        }
        Ok(Self {
            model_roles,
            permission,
        })
    }

    pub(crate) fn apply_to(&self, mut builder: RuntimeBuilder) -> RuntimeBuilder {
        for role in &self.model_roles {
            builder =
                builder.model_provider_for_role(role.role(), role.provider(), role.model().clone());
        }
        self.permission.apply_to(builder)
    }
}

/// Typed subagent composition policy for a parent coding runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodingSubagentsConfig {
    installed: bool,
    enabled: bool,
    limits: SubagentConfig,
}

impl CodingSubagentsConfig {
    /// Installs and enables subagent tools with the supplied runtime limits.
    #[must_use]
    pub const fn enabled(limits: SubagentConfig) -> Self {
        Self {
            installed: true,
            enabled: true,
            limits,
        }
    }

    /// Installs subagent tools while leaving their runtime admission disabled.
    #[must_use]
    pub const fn runtime_controlled(enabled: bool, limits: SubagentConfig) -> Self {
        Self {
            installed: true,
            enabled,
            limits,
        }
    }

    /// Returns whether the parent should advertise subagent tools.
    #[must_use]
    pub const fn is_installed(self) -> bool {
        self.installed
    }

    /// Returns whether new subagent work is enabled by the parent policy.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Returns the runtime-owned subagent limits.
    #[must_use]
    pub const fn limits(self) -> SubagentConfig {
        self.limits
    }
}

/// Typed parent coding-runtime inputs shared by full and read-only variants.
///
/// The full constructor requires a [`ProcessBackend`]. The read-only
/// constructor intentionally has no process backend, making it impossible for
/// the command-generation variant to install a process session by accident.
#[derive(Clone)]
pub struct CodingRuntimeInput {
    session_id: SessionId,
    root: PathBuf,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    process_backend: Option<Arc<dyn ProcessBackend>>,
    extra_tools: Vec<RegisteredTool>,
    allow_hidden_workspace_paths: bool,
    automatic_compaction: AutomaticCompactionConfig,
    retry_policy: Option<ModelRetryPolicy>,
    model_roles: Vec<CodingModelRoleConfig>,
    skill_roots: Vec<PathBuf>,
    subagents: CodingSubagentsConfig,
    workspace_tool_limits: WorkspaceToolLimits,
}

impl CodingRuntimeInput {
    /// Creates full coding-runtime inputs with a typed process backend.
    #[must_use]
    pub fn new(
        session_id: SessionId,
        root: impl Into<PathBuf>,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        process_backend: Arc<dyn ProcessBackend>,
    ) -> Self {
        Self {
            session_id,
            root: root.into(),
            provider,
            model,
            process_backend: Some(process_backend),
            extra_tools: Vec::new(),
            allow_hidden_workspace_paths: false,
            automatic_compaction: AutomaticCompactionConfig::default(),
            retry_policy: None,
            model_roles: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: WorkspaceToolLimits::default(),
        }
    }

    /// Creates read-only command-generation inputs without a process backend.
    #[must_use]
    pub fn read_only(
        session_id: SessionId,
        root: impl Into<PathBuf>,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
    ) -> Self {
        Self {
            session_id,
            root: root.into(),
            provider,
            model,
            process_backend: None,
            extra_tools: Vec::new(),
            allow_hidden_workspace_paths: false,
            automatic_compaction: AutomaticCompactionConfig::default(),
            retry_policy: None,
            model_roles: Vec::new(),
            skill_roots: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: WorkspaceToolLimits::default(),
        }
    }

    /// Adds provider-neutral tools owned by the calling surface or adapter.
    #[must_use]
    pub fn with_extra_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = RegisteredTool>,
    {
        self.extra_tools.extend(tools);
        self
    }

    /// Controls whether hidden workspace path components are readable.
    #[must_use]
    pub fn with_allow_hidden_workspace_paths(mut self, allow: bool) -> Self {
        self.allow_hidden_workspace_paths = allow;
        self
    }

    /// Sets automatic context compaction policy.
    #[must_use]
    pub fn with_automatic_compaction(mut self, config: AutomaticCompactionConfig) -> Self {
        self.automatic_compaction = config;
        self
    }

    /// Sets the model retry/recovery policy shared by parent and child runtimes.
    #[must_use]
    pub fn with_retry_policy(mut self, policy: ModelRetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }

    /// Adds one non-primary runtime model role.
    #[must_use]
    pub fn with_model_role(mut self, role: CodingModelRoleConfig) -> Self {
        self.model_roles.push(role);
        self
    }

    /// Adds model roles in the supplied deterministic order.
    #[must_use]
    pub fn with_model_roles<I>(mut self, roles: I) -> Self
    where
        I: IntoIterator<Item = CodingModelRoleConfig>,
    {
        self.model_roles.extend(roles);
        self
    }

    /// Sets the read-only skill/resource roots.
    #[must_use]
    pub fn with_skill_roots<I, P>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.skill_roots = roots.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the parent subagent policy.
    #[must_use]
    pub fn with_subagents(mut self, subagents: CodingSubagentsConfig) -> Self {
        self.subagents = subagents;
        self
    }

    /// Sets the workspace tool limits for the coding profile.
    #[must_use]
    pub fn with_workspace_tool_limits(mut self, limits: WorkspaceToolLimits) -> Self {
        self.workspace_tool_limits = limits;
        self
    }
}

/// Internal parent coding-runtime variant selected by the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodingRuntimeVariant {
    /// Full coding runtime with process, patch, plan, and optional subagent lanes.
    FullCoding,
    /// Read-only workspace runtime for command generation.
    ReadOnlyCommandGeneration,
}

/// Builder for parent coding runtime composition.
pub struct CodingRuntimeBuilder {
    input: CodingRuntimeInput,
    variant: CodingRuntimeVariant,
    permission: CodingPermissionPolicy,
}

impl CodingRuntimeBuilder {
    /// Creates a full coding-runtime builder.
    #[must_use]
    pub fn new(input: CodingRuntimeInput) -> Self {
        Self {
            input,
            variant: CodingRuntimeVariant::FullCoding,
            permission: CodingPermissionPolicy::default(),
        }
    }

    /// Creates the explicit read-only command-generation variant.
    ///
    /// Full-only inputs such as a process backend and subagent configuration
    /// are intentionally ignored by this variant. The read-only profile is
    /// selected by construction so those capabilities cannot be advertised or
    /// admitted accidentally.
    #[must_use]
    pub fn for_command_generation(input: CodingRuntimeInput) -> Self {
        Self {
            input,
            variant: CodingRuntimeVariant::ReadOnlyCommandGeneration,
            permission: CodingPermissionPolicy::default(),
        }
    }

    /// Sets the complete permission policy shared by parent and child runtimes.
    #[must_use]
    pub fn permission_policy(mut self, policy: CodingPermissionPolicy) -> Self {
        self.permission = policy;
        self
    }

    /// Builds a new parent runtime and its stable coding loop policy.
    pub fn build(self) -> Result<CodingRuntime, CodingRuntimeBuildError> {
        let (builder, profile) = self.compose()?;
        let loop_config = profile.loop_config()?;
        let runtime = builder
            .build()
            .map_err(|source| CodingRuntimeBuildError::RuntimeBuild { source })?;
        Ok(CodingRuntime {
            runtime,
            profile,
            loop_config,
        })
    }

    /// Resumes a parent runtime without automatic savepoints.
    pub async fn resume_from_store_without_automatic_savepoints(
        self,
        store: FileSessionStore,
    ) -> Result<CodingRuntime, CodingRuntimeBuildError> {
        let (builder, profile) = self.compose()?;
        let loop_config = profile.loop_config()?;
        let runtime = builder
            .resume_from_store_without_automatic_savepoints(store)
            .await
            .map_err(|source| CodingRuntimeBuildError::RuntimeBuild { source })?;
        Ok(CodingRuntime {
            runtime,
            profile,
            loop_config,
        })
    }

    fn compose(
        self,
    ) -> Result<(merry_runtime::RuntimeBuilder, CodingAgentProfile), CodingRuntimeBuildError> {
        let Self {
            input,
            variant,
            permission,
        } = self;
        let CodingRuntimeInput {
            session_id,
            root,
            provider,
            model,
            process_backend,
            extra_tools,
            allow_hidden_workspace_paths,
            automatic_compaction,
            retry_policy,
            model_roles,
            skill_roots,
            subagents,
            workspace_tool_limits,
        } = input;

        let policy = CodingRuntimePolicy::try_new(model_roles, permission)?;

        let project_rules = load_root_project_rules(&root)?;
        let skill_catalog = if skill_roots.is_empty() {
            None
        } else {
            Some(SkillCatalog::load_from_roots(skill_roots.clone())?)
        };
        if let Some(catalog) = skill_catalog.as_ref() {
            const MAX_LOGGED_SKILL_NAMES: usize = 64;
            let skill_names = catalog
                .skills()
                .iter()
                .take(MAX_LOGGED_SKILL_NAMES)
                .map(|skill| skill.name())
                .collect::<Vec<_>>();
            tracing::info!(
                event = "runtime.skill_catalog.load",
                session_id = %session_id,
                configured_root_count = skill_roots.len(),
                readable_root_count = skill_roots.iter().filter(|root| root.is_dir()).count(),
                skill_count = catalog.skills().len(),
                warning_count = catalog.warnings().len(),
                skill_names = ?skill_names,
                "runtime skill catalog loaded"
            );
        }
        let mut runtime_builder = Runtime::builder(session_id.clone())
            .automatic_compaction(automatic_compaction)
            .model_provider(Arc::clone(&provider), model.clone());
        if matches!(variant, CodingRuntimeVariant::FullCoding) {
            runtime_builder = runtime_builder.coordinator_plan_tools();
        }

        let mut profile_builder: CodingAgentProfileBuilder = coding_agent(&root)
            .readonly_resource_roots(skill_roots.clone())
            .allow_hidden(allow_hidden_workspace_paths)
            .limits(workspace_tool_limits)
            .register_tools(extra_tools);
        if let Some(project_rules) = project_rules {
            profile_builder = profile_builder.project_rules(project_rules);
        }
        if let Some(skill_catalog) = skill_catalog {
            profile_builder = profile_builder.skill_catalog(skill_catalog);
        }
        if let Some(policy) = retry_policy {
            profile_builder = profile_builder.retry_policy(policy);
        }

        let mut profile_tools = Vec::new();
        if matches!(variant, CodingRuntimeVariant::FullCoding) {
            let process_backend =
                process_backend.ok_or(CodingRuntimeBuildError::MissingProcessBackend)?;
            let process_session = process_backend.new_session();
            profile_builder = profile_builder
                .patch_tool()
                .accepted_process_session(process_session);

            if subagents.is_installed() {
                tracing::info!(
                    event = "runtime.subagents.enabled",
                    session_id = %session_id,
                    enabled = subagents.is_enabled(),
                    max_threads = subagents.limits().max_threads(),
                    max_depth = subagents.limits().max_depth(),
                    max_model_turns = subagents.limits().max_model_turns(),
                    "runtime subagent tools installed"
                );
                let composition = CodingRuntimeComposition {
                    profile_builder: profile_builder.clone(),
                    provider: Arc::clone(&provider),
                    model: model.clone(),
                    process_backend,
                    subagent_config: subagents.limits(),
                    automatic_compaction,
                    policy: policy.clone(),
                };
                let child_factory: Arc<dyn ChildRuntimeFactory> =
                    Arc::new(CodingChildRuntimeFactory::from_composition(composition));
                let manager = SubagentManager::runtime_controlled(
                    session_id.clone(),
                    subagents.limits(),
                    child_factory,
                    subagents.is_enabled(),
                );
                let [spawn_tool, wait_tool, cancel_tool] =
                    subagent_registered_tools(manager.clone())?;
                runtime_builder = runtime_builder.subagent_manager(manager);
                profile_tools.extend([spawn_tool, wait_tool, cancel_tool]);
            }
        }

        let profile = profile_builder.register_tools(profile_tools).build()?;
        runtime_builder = profile
            .apply_to(runtime_builder)
            .map_err(|source| CodingRuntimeBuildError::RuntimeProfileApply { source })?;
        runtime_builder = policy.apply_to(runtime_builder);

        Ok((runtime_builder, profile))
    }
}

/// A built parent coding runtime and the policy that governs its loop.
pub struct CodingRuntime {
    runtime: Runtime,
    profile: CodingAgentProfile,
    loop_config: AgentLoopConfig,
}

impl CodingRuntime {
    /// Borrows the runtime state handle owned by this composition result.
    #[must_use]
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Consumes the composition result and returns its runtime handle.
    #[must_use]
    pub fn into_runtime(self) -> Runtime {
        self.runtime
    }

    /// Borrows the shared coding profile used by this runtime.
    #[must_use]
    pub fn profile(&self) -> &CodingAgentProfile {
        &self.profile
    }

    /// Clones the loop configuration derived from the shared coding profile.
    #[must_use]
    pub fn loop_config(&self) -> AgentLoopConfig {
        self.loop_config.clone()
    }
}

/// Errors raised while composing a parent coding runtime.
#[derive(Debug, Error)]
pub enum CodingRuntimeBuildError {
    /// A full coding runtime was requested without its process backend.
    #[error("full coding runtime requires a process backend")]
    MissingProcessBackend,
    /// A role was configured more than once.
    #[error("coding model role {role:?} was configured more than once")]
    DuplicateModelRole { role: RuntimeModelRole },
    /// A provider-neutral protocol value failed validation.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// Project rules could not be loaded safely.
    #[error(transparent)]
    ProjectRules(#[from] ProjectRulesLoadError),
    /// The skill catalog could not be loaded.
    #[error(transparent)]
    SkillCatalog(#[from] SkillError),
    /// The coding profile was invalid.
    #[error(transparent)]
    CodingProfile(#[from] CodingAgentProfileBuildError),
    /// The profile's coding loop policy was invalid.
    #[error(transparent)]
    LoopConfig(#[from] AgentLoopConfigError),
    /// Applying the profile to the runtime builder failed.
    #[error("failed to apply coding profile to runtime builder: {source}")]
    RuntimeProfileApply { source: RuntimeError },
    /// Runtime construction or resume failed.
    #[error("failed to build coding runtime: {source}")]
    RuntimeBuild { source: RuntimeError },
}
