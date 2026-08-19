use super::CodingRuntimeError;
use super::process::ActionProcessBackend;
use super::roles::RuntimeRoleProviderConfig;
use merry::coding_agent_loop_config as shared_coding_agent_loop_config;
use merry::profiles::{
    CodingAgentProfileBuilder, CodingChildRuntimeFactory, coding_agent, load_root_project_rules,
};
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use merry_runtime::{
    AutomaticCompactionConfig, ChildRuntimeFactory, FileSessionStore, PermissionAdmissionSource,
    PermissionReviewMode, RegisteredTool, Runtime, RuntimeBuilder, SubagentConfig, SubagentManager,
    subagent_registered_tools,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) fn coding_agent_loop_config()
-> Result<merry_runtime::AgentLoopConfig, CodingRuntimeError> {
    shared_coding_agent_loop_config().map_err(CodingRuntimeError::from)
}

pub(crate) struct CodingRuntimeOptions {
    pub(crate) allow_hidden_workspace_paths: bool,
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) automatic_compaction: AutomaticCompactionConfig,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) process_backend: ActionProcessBackend,
    pub(crate) extra_tools: Vec<RegisteredTool>,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) subagents: CodingSubagentsConfig,
    pub(crate) workspace_tool_limits: Option<merry_tool_workspace::WorkspaceToolLimits>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodingSubagentsConfig {
    installed: bool,
    enabled: bool,
    limits: SubagentConfig,
}

impl CodingSubagentsConfig {
    pub(crate) fn enabled(limits: SubagentConfig) -> Self {
        Self {
            installed: true,
            enabled: true,
            limits,
        }
    }

    pub(crate) fn runtime_controlled(enabled: bool, limits: SubagentConfig) -> Self {
        Self {
            installed: true,
            enabled,
            limits,
        }
    }

    pub(crate) fn is_installed(self) -> bool {
        self.installed
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
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
    pub(crate) process_backend: ActionProcessBackend,
    pub(crate) extra_tools: Vec<RegisteredTool>,
    pub(crate) allow_hidden_workspace_paths: bool,
    pub(crate) automatic_compaction: AutomaticCompactionConfig,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) subagents: CodingSubagentsConfig,
}

pub(crate) fn build_headless_coding(
    input: HeadlessCodingRuntimeInput<'_>,
) -> Result<Runtime, CodingRuntimeError> {
    build_coding_runtime_from_headless_input(input, None, None)?
        .build()
        .map_err(|source| CodingRuntimeError::RuntimeBuild { source })
}

pub(crate) fn build_headless_coding_with_permission_review_mode(
    input: HeadlessCodingRuntimeInput<'_>,
    mode: PermissionReviewMode,
) -> Result<Runtime, CodingRuntimeError> {
    build_coding_runtime_from_headless_input(input, None, Some(mode))?
        .build()
        .map_err(|source| CodingRuntimeError::RuntimeBuild { source })
}

pub(crate) fn build_headless_coding_with_permission_source(
    input: HeadlessCodingRuntimeInput<'_>,
    source: Arc<dyn PermissionAdmissionSource>,
    mode: PermissionReviewMode,
) -> Result<Runtime, CodingRuntimeError> {
    build_coding_runtime_from_headless_input(input, Some((source, mode)), None)?
        .build()
        .map_err(|source| CodingRuntimeError::RuntimeBuild { source })
}

#[allow(dead_code)]
pub(crate) async fn resume_headless_coding(
    input: HeadlessCodingRuntimeInput<'_>,
    store: FileSessionStore,
) -> Result<Runtime, CodingRuntimeError> {
    build_coding_runtime_from_headless_input(input, None, None)?
        .resume_from_store_without_automatic_savepoints(store)
        .await
        .map_err(|source| CodingRuntimeError::RuntimeBuild { source })
}

pub(crate) async fn resume_headless_coding_with_permission_source(
    input: HeadlessCodingRuntimeInput<'_>,
    store: FileSessionStore,
    source: Arc<dyn PermissionAdmissionSource>,
    mode: PermissionReviewMode,
) -> Result<Runtime, CodingRuntimeError> {
    build_coding_runtime_from_headless_input(input, Some((source, mode)), None)?
        .resume_from_store_without_automatic_savepoints(store)
        .await
        .map_err(|source| CodingRuntimeError::RuntimeBuild { source })
}

fn build_coding_runtime_from_headless_input(
    input: HeadlessCodingRuntimeInput<'_>,
    permission_source: Option<(Arc<dyn PermissionAdmissionSource>, PermissionReviewMode)>,
    permission_review_mode: Option<PermissionReviewMode>,
) -> Result<RuntimeBuilder, CodingRuntimeError> {
    let mut builder = configure_coding_runtime_builder(
        input.session_id,
        input.root,
        input.provider,
        input.model,
        CodingRuntimeOptions {
            allow_hidden_workspace_paths: input.allow_hidden_workspace_paths,
            approval_review: input.approval_review,
            automatic_compaction: input.automatic_compaction,
            retry_policy: input.retry_policy,
            context_compaction: input.context_compaction,
            process_backend: input.process_backend,
            extra_tools: input.extra_tools,
            skill_roots: input.skill_roots,
            subagents: input.subagents,
            workspace_tool_limits: None,
        },
    )?;
    if let Some(mode) = permission_review_mode {
        builder = builder.permission_review_mode(mode);
    }
    if let Some((source, mode)) = permission_source {
        builder = builder
            .permission_review_mode(mode)
            .permission_admission_source(source);
    }
    Ok(builder)
}

#[cfg(test)]
pub(crate) fn build_coding_runtime(
    session_id: &str,
    root: &Path,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    options: CodingRuntimeOptions,
) -> Result<Runtime, CodingRuntimeError> {
    configure_coding_runtime_builder(session_id, root, provider, model, options)?
        .build()
        .map_err(|source| CodingRuntimeError::RuntimeBuild { source })
}

fn configure_coding_runtime_builder(
    session_id: &str,
    root: &Path,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    options: CodingRuntimeOptions,
) -> Result<RuntimeBuilder, CodingRuntimeError> {
    let parent_session_id = merry_core::SessionId::new(session_id)?;
    let project_rules = load_root_project_rules(root)?;
    let process_backend = options.process_backend;
    let process_session = process_backend.new_session();
    let mut builder = Runtime::builder(parent_session_id.clone())
        .automatic_compaction(options.automatic_compaction)
        .coordinator_plan_tools()
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
    let skill_catalog = if !options.skill_roots.is_empty() {
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
        Some(catalog)
    } else {
        None
    };

    let mut profile_builder: CodingAgentProfileBuilder = coding_agent(root)
        .readonly_resource_roots(options.skill_roots.clone())
        .allow_hidden(options.allow_hidden_workspace_paths)
        .limits(options.workspace_tool_limits.unwrap_or_default())
        .patch_tool()
        .accepted_process_session(process_session.clone())
        .register_tools(options.extra_tools);
    if let Some(skill_catalog) = skill_catalog {
        profile_builder = profile_builder.skill_catalog(skill_catalog);
    }
    if let Some(project_rules) = project_rules {
        profile_builder = profile_builder.project_rules(project_rules);
    }

    let child_factory: Arc<dyn ChildRuntimeFactory> = Arc::new(CodingChildRuntimeFactory::new(
        profile_builder.clone(),
        Arc::clone(&provider),
        model.clone(),
        Arc::clone(&process_backend),
        options.subagents.limits(),
        options.automatic_compaction,
    ));

    let mut profile_tools = Vec::new();
    if options.subagents.is_installed() {
        let manager = SubagentManager::runtime_controlled(
            parent_session_id.clone(),
            options.subagents.limits(),
            child_factory,
            options.subagents.is_enabled(),
        );
        let [spawn_tool, wait_tool, cancel_tool] = subagent_registered_tools(manager.clone())?;
        builder = builder.subagent_manager(manager);
        profile_tools.extend([spawn_tool, wait_tool, cancel_tool]);
        tracing::info!(
            event = "runtime.subagents.enabled",
            session_id,
            enabled = options.subagents.is_enabled(),
            max_threads = options.subagents.limits().max_threads(),
            max_depth = options.subagents.limits().max_depth(),
            "runtime subagent tools registered"
        );
    }

    let mut profile = profile_builder.register_tools(profile_tools);
    if let Some(policy) = options.retry_policy {
        profile = profile.retry_policy(policy);
    }
    let profile = profile.build()?;
    let builder = profile
        .apply_to(builder)
        .map_err(|source| CodingRuntimeError::RuntimeProfileApply { source })?;
    Ok(builder)
}
