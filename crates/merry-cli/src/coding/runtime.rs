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
use merry_runtime::{AutomaticCompactionConfig, FileSessionStore, RegisteredTool};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
pub(crate) struct CodingRuntimeOptions {
    pub(crate) allow_hidden_workspace_paths: bool,
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) automatic_compaction: AutomaticCompactionConfig,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) process_backend: ActionProcessBackend,
    pub(crate) extra_tools: Vec<RegisteredTool>,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) subagents: super::CodingSubagentsConfig,
    pub(crate) workspace_tool_limits: Option<merry_tool_workspace::WorkspaceToolLimits>,
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
    pub(crate) subagents: super::CodingSubagentsConfig,
    pub(crate) workspace_tool_limits: Option<merry_tool_workspace::WorkspaceToolLimits>,
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
    build_headless_coding_with_policy_composition(input, CodingPermissionPolicy::default())
}

pub(crate) fn build_headless_coding_with_policy_composition(
    input: HeadlessCodingRuntimeInput<'_>,
    permission: CodingPermissionPolicy,
) -> Result<SharedCodingRuntime, CodingRuntimeError> {
    build_coding_runtime_from_headless_input(input, permission)?
        .build()
        .map_err(CodingRuntimeError::from)
}

pub(crate) async fn resume_headless_coding_composition_with_policy(
    input: HeadlessCodingRuntimeInput<'_>,
    store: FileSessionStore,
    permission: CodingPermissionPolicy,
) -> Result<SharedCodingRuntime, CodingRuntimeError> {
    build_coding_runtime_from_headless_input(input, permission)?
        .resume_from_store_without_automatic_savepoints(store)
        .await
        .map_err(CodingRuntimeError::from)
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
        allow_hidden_workspace_paths: options.allow_hidden_workspace_paths,
        automatic_compaction: options.automatic_compaction,
        retry_policy: options.retry_policy,
        context_compaction: options.context_compaction,
        approval_review: options.approval_review,
        skill_roots: options.skill_roots,
        subagents: options.subagents,
        workspace_tool_limits: options.workspace_tool_limits,
    })
}

fn build_coding_runtime_from_headless_input(
    input: HeadlessCodingRuntimeInput<'_>,
    permission: CodingPermissionPolicy,
) -> Result<CodingRuntimeBuilder, CodingRuntimeError> {
    let session_id = merry_core::SessionId::new(input.session_id)?;
    let mut coding_input = CodingRuntimeInput::new(
        session_id,
        input.root,
        input.provider,
        input.model,
        input.process_backend,
    )
    .with_extra_tools(input.extra_tools)
    .with_allow_hidden_workspace_paths(input.allow_hidden_workspace_paths)
    .with_automatic_compaction(input.automatic_compaction)
    .with_skill_roots(input.skill_roots)
    .with_subagents(input.subagents);
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
    Ok(CodingRuntimeBuilder::new(coding_input).permission_policy(permission))
}
