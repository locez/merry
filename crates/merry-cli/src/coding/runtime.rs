use super::CodingRuntimeError;
use super::RuntimeRoleProviderConfig;
use super::process::ActionProcessBackend;
use merry::profiles::{
    CodingPermissionPolicy, CodingRuntime as SharedCodingRuntime, CodingRuntimeBuilder,
    CodingRuntimeInput,
};
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
#[cfg(test)]
use merry_runtime::Runtime;
use merry_runtime::{
    CompactionConfig, FileSessionStore, HistoryRetention, LoadedSession, PermissionReviewRules,
    RegisteredTool,
};
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
    pub(crate) workspace_tool_limits: Option<merry_tools::WorkspaceToolLimits>,
}

pub(crate) struct HeadlessCodingRuntimeInput<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) root: &'a Path,
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
    pub(crate) process_backend: ActionProcessBackend,
    pub(crate) extra_tools: Vec<RegisteredTool>,
    pub(crate) automatic_compaction: CompactionConfig,
    pub(crate) history_retention: HistoryRetention,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) denied_absolute_paths: Vec<PathBuf>,
    pub(crate) subagents: super::CodingSubagentsConfig,
    pub(crate) workspace_tool_limits: Option<merry_tools::WorkspaceToolLimits>,
    /// When false, `AGENTS.md` stays unloaded and `apply_patch` cannot write.
    pub(crate) workspace_writable: bool,
}

#[cfg(test)]
pub(crate) fn build_headless_coding(
    input: HeadlessCodingRuntimeInput<'_>,
) -> Result<Runtime, CodingRuntimeError> {
    build_headless_coding_composition(input).map(SharedCodingRuntime::into_runtime)
}

#[cfg(test)]
pub(crate) fn build_headless_coding_composition(
    input: HeadlessCodingRuntimeInput<'_>,
) -> Result<SharedCodingRuntime, CodingRuntimeError> {
    build_headless_coding_with_policy_composition(
        input,
        CodingPermissionPolicy::default(),
        PermissionReviewRules::default(),
        None,
        None,
    )
}

pub(crate) fn build_headless_coding_with_policy_composition(
    input: HeadlessCodingRuntimeInput<'_>,
    permission: CodingPermissionPolicy,
    review_rules: PermissionReviewRules,
    session_store: Option<FileSessionStore>,
    max_parallel_tool_calls: Option<std::num::NonZeroUsize>,
) -> Result<SharedCodingRuntime, CodingRuntimeError> {
    let mut builder = build_coding_runtime_from_headless_input(
        input,
        permission,
        review_rules,
        max_parallel_tool_calls,
    )?;
    if let Some(store) = session_store {
        builder = builder.session_store(store);
    }
    builder.build().map_err(CodingRuntimeError::from)
}

#[cfg(test)]
async fn resume_headless_coding_composition_with_policy(
    input: HeadlessCodingRuntimeInput<'_>,
    store: FileSessionStore,
    permission: CodingPermissionPolicy,
) -> Result<SharedCodingRuntime, CodingRuntimeError> {
    build_coding_runtime_from_headless_input(
        input,
        permission,
        PermissionReviewRules::default(),
        None,
    )?
    .resume_from_store_without_automatic_savepoints(store)
    .await
    .map_err(CodingRuntimeError::from)
}

pub(crate) fn resume_headless_coding_composition_with_loaded_session(
    input: HeadlessCodingRuntimeInput<'_>,
    loaded: LoadedSession,
    permission: CodingPermissionPolicy,
    review_rules: PermissionReviewRules,
    session_store: Option<FileSessionStore>,
    max_parallel_tool_calls: Option<std::num::NonZeroUsize>,
) -> Result<SharedCodingRuntime, CodingRuntimeError> {
    let mut builder = build_coding_runtime_from_headless_input(
        input,
        permission,
        review_rules,
        max_parallel_tool_calls,
    )?
    .with_loaded_session(loaded);
    if let Some(store) = session_store {
        builder = builder.session_store(store);
    }
    builder.build().map_err(CodingRuntimeError::from)
}

#[allow(dead_code)]
#[cfg(test)]
pub(crate) async fn resume_headless_coding(
    input: HeadlessCodingRuntimeInput<'_>,
    store: FileSessionStore,
) -> Result<Runtime, CodingRuntimeError> {
    resume_headless_coding_composition(input, store)
        .await
        .map(SharedCodingRuntime::into_runtime)
}

#[cfg(test)]
async fn resume_headless_coding_composition(
    input: HeadlessCodingRuntimeInput<'_>,
    store: FileSessionStore,
) -> Result<SharedCodingRuntime, CodingRuntimeError> {
    resume_headless_coding_composition_with_policy(input, store, CodingPermissionPolicy::default())
        .await
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

fn build_coding_runtime_from_headless_input(
    input: HeadlessCodingRuntimeInput<'_>,
    permission: CodingPermissionPolicy,
    review_rules: PermissionReviewRules,
    max_parallel_tool_calls: Option<std::num::NonZeroUsize>,
) -> Result<CodingRuntimeBuilder, CodingRuntimeError> {
    let session_id = merry_core::SessionId::new(input.session_id)?;
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
    .with_extra_tools(input.extra_tools)
    .with_automatic_compaction(input.automatic_compaction)
    .with_history_retention(input.history_retention)
    .with_skill_roots(input.skill_roots)
    .with_denied_absolute_paths(input.denied_absolute_paths)
    .with_subagents(input.subagents)
    .with_permission_review_rules(review_rules);
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
    if let Ok(role) = merry::profiles::CodingModelRoleConfig::new(
        merry_runtime::RuntimeModelRole::SummaryMemory,
        summary_provider,
        summary_model,
    ) {
        coding_input = coding_input.with_model_role(role);
    }
    if !input.workspace_writable {
        coding_input = coding_input.read_only_workspace();
    }
    let coding_input = attach_durable_memory(coding_input, input.root);
    Ok(CodingRuntimeBuilder::new(coding_input).permission_policy(permission))
}

/// Opens XDG durable memory for a production runtime.
///
/// Unit tests leave memory unset so they do not touch the user data directory.
/// The `merry` binary, including integration tests, always tries to open it.
#[cfg(not(test))]
pub(crate) fn attach_durable_memory(input: CodingRuntimeInput, root: &Path) -> CodingRuntimeInput {
    match merry_runtime::DurableMemory::open_from_env(root) {
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

#[cfg(test)]
pub(crate) fn attach_durable_memory(input: CodingRuntimeInput, _root: &Path) -> CodingRuntimeInput {
    input
}
