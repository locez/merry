use super::CodingRuntimeError;
use super::child_runtime::CodingLoopChildRuntimeFactory;
use super::process::ActionProcessBackend;
use super::profile::{
    coding_loop_workspace_roots, with_workspace_coding_loop_profile, workspace_tools_config,
};
use super::roles::RuntimeRoleProviderConfig;
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AutomaticCompactionConfig,
    DEFAULT_CODING_AGENT_MAX_MODEL_TURNS, PermissionedProcessRunnerFactory, ProcessRunner, Runtime,
    SubagentConfig, SubagentManager, subagent_registered_tools,
};
use merry_tool_workspace::WorkspaceCodingLoopProfile;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) fn coding_agent_loop_config()
-> Result<merry_runtime::AgentLoopConfig, CodingRuntimeError> {
    merry_runtime::AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS)
        .map_err(CodingRuntimeError::from)
}

pub(crate) struct CodingLoopRuntimeOptions {
    pub(crate) allow_hidden_workspace_paths: bool,
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) automatic_compaction: AutomaticCompactionConfig,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) permissioned_process_runner_factory:
        Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) subagents: CodingSubagentsConfig,
    pub(crate) workspace_tool_limits: Option<merry_tool_workspace::WorkspaceToolLimits>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodingSubagentsConfig {
    enabled: bool,
    limits: SubagentConfig,
}

impl CodingSubagentsConfig {
    pub(crate) fn enabled(limits: SubagentConfig) -> Self {
        Self {
            enabled: true,
            limits,
        }
    }

    pub(crate) fn is_enabled(self) -> bool {
        self.enabled
    }

    pub(crate) fn limits(self) -> SubagentConfig {
        self.limits
    }
}

pub(crate) struct HeadlessCodingRuntimeInput<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) root: &'a Path,
    pub(crate) admission: AcceptedLocalWorkspaceProcessAdmission,
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
    pub(crate) runner: Arc<dyn ProcessRunner>,
    pub(crate) permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    pub(crate) allow_hidden_workspace_paths: bool,
    pub(crate) automatic_compaction: AutomaticCompactionConfig,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) subagents: CodingSubagentsConfig,
}

pub(crate) fn build_headless_coding_runtime(
    input: HeadlessCodingRuntimeInput<'_>,
) -> Result<Runtime, CodingRuntimeError> {
    build_coding_loop_runtime(
        input.session_id,
        input.root,
        input.admission,
        input.provider,
        input.model,
        input.runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: input.allow_hidden_workspace_paths,
            approval_review: input.approval_review,
            automatic_compaction: input.automatic_compaction,
            retry_policy: input.retry_policy,
            context_compaction: input.context_compaction,
            permissioned_process_runner_factory: Some(input.permissioned_process_runner_factory),
            skill_roots: input.skill_roots,
            subagents: input.subagents,
            workspace_tool_limits: None,
        },
    )
}

pub(crate) fn build_coding_loop_runtime(
    session_id: &str,
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    runner: Arc<dyn ProcessRunner>,
    options: CodingLoopRuntimeOptions,
) -> Result<Runtime, CodingRuntimeError> {
    let parent_session_id = merry_core::SessionId::new(session_id)?;
    let permissioned_factory = options
        .permissioned_process_runner_factory
        .unwrap_or_else(|| {
            Arc::new(merry_runtime::StaticPermissionedProcessRunnerFactory::new(
                Arc::clone(&runner),
            ))
        });
    let mut builder = Runtime::builder(parent_session_id.clone())
        .automatic_compaction(options.automatic_compaction)
        .model_provider(Arc::clone(&provider), model.clone());
    if let Some(role_provider) = options.context_compaction {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    if let Some(role_provider) = options.approval_review {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    if !options.skill_roots.is_empty() {
        let catalog = merry_runtime::SkillCatalog::load_from_roots(options.skill_roots.clone())?;
        let skill_names = catalog
            .skills()
            .iter()
            .map(|skill| skill.name())
            .collect::<Vec<_>>();
        let skill_paths = catalog
            .skills()
            .iter()
            .map(|skill| skill.skill_md_path().display().to_string())
            .collect::<Vec<_>>();
        tracing::info!(
            event = "runtime.skill_catalog.load",
            session_id,
            configured_root_count = options.skill_roots.len(),
            readable_root_count = options
                .skill_roots
                .iter()
                .filter(|root| root.is_dir())
                .count(),
            skill_count = catalog.skills().len(),
            warning_count = catalog.warnings().len(),
            skill_names = ?skill_names,
            skill_paths = ?skill_paths,
            "runtime skill catalog loaded"
        );
        builder = builder.skill_catalog(catalog);
    }

    if options.subagents.is_enabled() {
        let factory = CodingLoopChildRuntimeFactory::new(
            root,
            admission,
            Arc::clone(&provider),
            model.clone(),
            ActionProcessBackend::from_parts(
                Arc::clone(&runner),
                Arc::clone(&permissioned_factory),
            ),
            options.skill_roots.clone(),
            options.allow_hidden_workspace_paths,
        );
        let manager = SubagentManager::new(
            parent_session_id.clone(),
            options.subagents.limits(),
            Arc::new(factory),
        );
        let [spawn_tool, wait_tool, cancel_tool] = subagent_registered_tools(manager.clone())?;
        builder = builder
            .subagent_manager(manager)
            .register_tool(spawn_tool)
            .register_tool(wait_tool)
            .register_tool(cancel_tool);
        tracing::info!(
            event = "runtime.subagents.enabled",
            session_id,
            max_threads = options.subagents.limits().max_threads(),
            max_depth = options.subagents.limits().max_depth(),
            "runtime subagent tools registered"
        );
    }

    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(root, &options.skill_roots),
        options.allow_hidden_workspace_paths,
        options.workspace_tool_limits,
    )?)
    .map_err(CodingRuntimeError::from)?
    .with_patch_tool()
    .with_cli_bwrap_permissioned_process_runner(admission, runner, permissioned_factory);
    let mut builder = with_workspace_coding_loop_profile(builder, profile)?;
    if let Some(policy) = options.retry_policy {
        builder = builder.model_retry_policy(policy);
    }
    builder
        .build()
        .map_err(|source| CodingRuntimeError::RuntimeBuild { source })
}
