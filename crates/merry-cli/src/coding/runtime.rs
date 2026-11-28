use super::CodingRuntimeError;
use super::RuntimeRoleProviderConfig;
use super::process::ActionProcessBackend;
#[cfg(test)]
use merry::FileSessionStore;
#[cfg(not(test))]
use merry::coding::{CodingModelRoleConfig, RuntimeModelRole};
use merry::coding::{CodingPermissionPolicy, CodingRuntimeInput, CodingSessionBuilder};
#[cfg(test)]
use merry::coding::{CodingRuntime as SharedCodingRuntime, CodingRuntimeBuilder};
use merry::permissions::PermissionReviewRules;
use merry::providers::{ModelName, ModelProvider, ModelRetryPolicy};
#[cfg(test)]
use merry::runtime::{RegisteredTool, Runtime};
use merry::{CompactionConfig, HistoryRetention};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
pub(crate) struct CodingRuntimeOptions {
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) automatic_compaction: CompactionConfig,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) process_backend: ActionProcessBackend,
    pub(crate) extra_tools: Vec<RegisteredTool>,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) subagents: super::CodingSubagentsConfig,
    pub(crate) workspace_tool_limits: Option<merry::profiles::WorkspaceToolLimits>,
}

pub(crate) struct HeadlessCodingRuntimeInput<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) root: &'a Path,
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
    pub(crate) process_backend: ActionProcessBackend,
    #[cfg(test)]
    pub(crate) extra_tools: Vec<RegisteredTool>,
    pub(crate) automatic_compaction: CompactionConfig,
    pub(crate) history_retention: HistoryRetention,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) denied_absolute_paths: Vec<PathBuf>,
    pub(crate) subagents: super::CodingSubagentsConfig,
    pub(crate) workspace_tool_limits: Option<merry::profiles::WorkspaceToolLimits>,
    /// When false, `AGENTS.md` stays unloaded and `apply_patch` cannot write.
    pub(crate) workspace_writable: bool,
}

pub(crate) fn build_coding_session_builder(
    input: HeadlessCodingRuntimeInput<'_>,
    permission: CodingPermissionPolicy,
    review_rules: PermissionReviewRules,
    max_parallel_tool_calls: Option<std::num::NonZeroUsize>,
) -> Result<CodingSessionBuilder, CodingRuntimeError> {
    let coding_input = build_coding_runtime_input(input, review_rules, max_parallel_tool_calls)?;
    let builder = CodingSessionBuilder::new(coding_input).permission_policy(permission);
    #[cfg(not(test))]
    let builder = builder.with_durable_memory_from_env();
    Ok(builder)
}

#[cfg(test)]
pub(crate) fn build_headless_coding(
    input: HeadlessCodingRuntimeInput<'_>,
) -> Result<Runtime, CodingRuntimeError> {
    build_headless_coding_with_policy_composition(
        input,
        CodingPermissionPolicy::default(),
        PermissionReviewRules::default(),
        None,
        None,
    )
    .map(SharedCodingRuntime::into_runtime)
}

#[cfg(test)]
pub(crate) fn build_headless_coding_with_policy_composition(
    input: HeadlessCodingRuntimeInput<'_>,
    permission: CodingPermissionPolicy,
    review_rules: PermissionReviewRules,
    session_store: Option<FileSessionStore>,
    max_parallel_tool_calls: Option<std::num::NonZeroUsize>,
) -> Result<SharedCodingRuntime, CodingRuntimeError> {
    let coding_input = build_coding_runtime_input(input, review_rules, max_parallel_tool_calls)?;
    let mut builder = CodingRuntimeBuilder::new(coding_input).permission_policy(permission);
    if let Some(store) = session_store {
        builder = builder.session_store(store);
    }
    builder.build().map_err(CodingRuntimeError::from)
}

#[cfg(test)]
pub(crate) async fn resume_headless_coding(
    input: HeadlessCodingRuntimeInput<'_>,
    store: FileSessionStore,
) -> Result<Runtime, CodingRuntimeError> {
    let coding_input = build_coding_runtime_input(input, PermissionReviewRules::default(), None)?;
    CodingRuntimeBuilder::new(coding_input)
        .permission_policy(CodingPermissionPolicy::default())
        .resume_from_store_without_automatic_savepoints(store)
        .await
        .map(SharedCodingRuntime::into_runtime)
        .map_err(CodingRuntimeError::from)
}

#[cfg(test)]
pub(crate) fn build_coding_runtime(
    session_id: &str,
    root: &Path,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    options: CodingRuntimeOptions,
) -> Result<Runtime, CodingRuntimeError> {
    build_headless_coding(HeadlessCodingRuntimeInput {
        session_id,
        root,
        provider,
        model,
        process_backend: options.process_backend,
        extra_tools: options.extra_tools,
        automatic_compaction: options.automatic_compaction,
        history_retention: HistoryRetention::default(),
        retry_policy: options.retry_policy,
        context_compaction: options.context_compaction,
        approval_review: options.approval_review,
        skill_roots: options.skill_roots,
        denied_absolute_paths: Vec::new(),
        subagents: options.subagents,
        workspace_tool_limits: options.workspace_tool_limits,
        workspace_writable: true,
    })
}

fn build_coding_runtime_input(
    input: HeadlessCodingRuntimeInput<'_>,
    review_rules: PermissionReviewRules,
    max_parallel_tool_calls: Option<std::num::NonZeroUsize>,
) -> Result<CodingRuntimeInput, CodingRuntimeError> {
    let session_id = merry::events::SessionId::new(input.session_id)?;
    #[cfg(not(test))]
    let summary_provider = Arc::clone(&input.provider);
    #[cfg(not(test))]
    let summary_model = input.model.clone();
    let mut coding_input = CodingRuntimeInput::new(
        session_id,
        input.root,
        input.provider,
        input.model,
        input.process_backend,
    )
    .with_automatic_compaction(input.automatic_compaction)
    .with_history_retention(input.history_retention)
    .with_skill_roots(input.skill_roots)
    .with_protected_host_paths(&merry::paths::ProtectedHostPaths::new(
        input.denied_absolute_paths,
    ))
    .with_subagents(input.subagents)
    .with_permission_review_rules(review_rules);
    #[cfg(test)]
    {
        coding_input = coding_input.with_extra_tools(input.extra_tools);
    }
    if let Some(limit) = max_parallel_tool_calls {
        coding_input = coding_input.with_max_parallel_tool_calls(limit);
    }
    if let Some(limits) = input.workspace_tool_limits {
        coding_input = coding_input.with_workspace_tool_limits(limits);
    }
    if let Some(policy) = input.retry_policy {
        coding_input = coding_input.with_retry_policy(policy);
    }
    if let Some(role) = input.context_compaction {
        coding_input = coding_input.with_model_role(role);
    }
    if let Some(role) = input.approval_review {
        coding_input = coding_input.with_model_role(role);
    }
    // Library tests keep the primary provider's script for the coding turn.
    // The product binary names the session with a separate summary call.
    #[cfg(not(test))]
    if let Ok(role) = CodingModelRoleConfig::new(
        RuntimeModelRole::SummaryMemory,
        summary_provider,
        summary_model,
    ) {
        coding_input = coding_input.with_model_role(role);
    }
    if !input.workspace_writable {
        coding_input = coding_input.read_only_workspace();
    }
    Ok(coding_input)
}
