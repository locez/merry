use crate::cli_error::{CliError, unexpected};
use crate::coding_runtime::{
    ActionProcessBackend, CodingLoopRuntimeOptions, build_coding_loop_runtime,
    coding_loop_workspace_roots, with_workspace_coding_loop_profile, workspace_tools_config,
};
use crate::provider_config::{
    OpenAiProviderConfigBundle, RuntimePrimaryProviderConfig, RuntimeProviderBundle,
    openai_provider_bundle,
};
use merry_llm::ModelName;
#[cfg(test)]
use merry_runtime::RuntimeModelRole;
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AutomaticCompactionConfig,
    PermissionedProcessRunnerFactory, ProcessRunner, Runtime,
};
use merry_tool_workspace::{WorkspaceCodingLoopProfile, WorkspaceToolLimits};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use super::{
    CODING_LOOP_LIVE_SMOKE_SESSION_ID, CODING_LOOP_SMOKE_SESSION_ID,
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID, CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID,
    CODING_LOOP_TASK_SMOKE_SESSION_ID, CodingLoopSmokeProvider, CodingLoopTaskSmokeFixture,
    CodingLoopTaskSmokeProvider, PERMISSION_NETWORK_SMOKE_SESSION_ID,
};
#[cfg(test)]
use super::{PermissionNetworkSmokeProvider, PermissionNetworkSmokeReviewProvider};

pub(crate) fn build_coding_loop_smoke_runtime(
    root: &Path,
    relative_cwd: Option<&str>,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    process_backend: Option<ActionProcessBackend>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let provider = CodingLoopSmokeProvider::new(relative_cwd)?;
    Ok(build_coding_loop_runtime(
        CODING_LOOP_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new("merry-coding-loop-smoke").map_err(unexpected)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction,
            retry_policy: None,
            context_compaction: None,
            process_backend,
            permissioned_process_runner_factory,
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: Default::default(),
            workspace_tool_limits: None,
        },
    )?)
}

pub(crate) fn build_coding_loop_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiProviderConfigBundle,
    runner: Arc<dyn ProcessRunner>,
    process_backend: Option<ActionProcessBackend>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let providers = openai_provider_bundle(config, unexpected)?;
    Ok(build_coding_loop_runtime(
        CODING_LOOP_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        providers.primary.provider,
        providers.primary.model,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: true,
            approval_review: providers.approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: providers.retry_policy,
            context_compaction: providers.context_compaction,
            process_backend,
            permissioned_process_runner_factory,
            extra_tools: Vec::new(),
            skill_roots: options.skill_roots,
            subagents: options.subagents,
            workspace_tool_limits: None,
        },
    )?)
}

#[allow(
    clippy::too_many_arguments,
    reason = "task smoke construction keeps its explicit runtime inputs at one boundary"
)]
pub(crate) fn build_coding_loop_task_smoke_runtime(
    root: &Path,
    relative_cwd: Option<&str>,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    process_backend: Option<ActionProcessBackend>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    fixture: CodingLoopTaskSmokeFixture,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let provider = CodingLoopTaskSmokeProvider::new(relative_cwd, fixture)?;
    Ok(build_coding_loop_runtime(
        CODING_LOOP_TASK_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new("merry-coding-loop-task-smoke").map_err(unexpected)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: None,
            automatic_compaction,
            retry_policy: None,
            context_compaction: None,
            process_backend,
            permissioned_process_runner_factory,
            extra_tools: Vec::new(),
            skill_roots: Vec::new(),
            subagents: Default::default(),
            workspace_tool_limits: Some(task_smoke_workspace_tool_limits()),
        },
    )?)
}

pub(crate) fn build_coding_loop_task_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiProviderConfigBundle,
    runner: Arc<dyn ProcessRunner>,
    process_backend: Option<ActionProcessBackend>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let providers = openai_provider_bundle(config, unexpected)?;
    Ok(build_coding_loop_runtime(
        CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        providers.primary.provider,
        providers.primary.model,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: true,
            approval_review: providers.approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: providers.retry_policy,
            context_compaction: providers.context_compaction,
            process_backend,
            permissioned_process_runner_factory,
            extra_tools: Vec::new(),
            skill_roots: options.skill_roots,
            subagents: options.subagents,
            workspace_tool_limits: Some(task_smoke_workspace_tool_limits()),
        },
    )?)
}

pub(crate) fn build_coding_loop_subagent_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiProviderConfigBundle,
    runner: Arc<dyn ProcessRunner>,
    process_backend: Option<ActionProcessBackend>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let providers = openai_provider_bundle(config, unexpected)?;
    Ok(build_coding_loop_runtime(
        CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        providers.primary.provider,
        providers.primary.model,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review: providers.approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: providers.retry_policy,
            context_compaction: providers.context_compaction,
            process_backend,
            permissioned_process_runner_factory,
            extra_tools: Vec::new(),
            skill_roots: options.skill_roots,
            subagents: options.subagents,
            workspace_tool_limits: None,
        },
    )?)
}

pub(crate) fn build_permission_network_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiProviderConfigBundle,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let session_id =
        merry_core::SessionId::new(PERMISSION_NETWORK_SMOKE_SESSION_ID).map_err(unexpected)?;
    let RuntimeProviderBundle {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    } = openai_provider_bundle(config, unexpected)?;
    let RuntimePrimaryProviderConfig { provider, model } = primary;
    let mut builder = Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction)
        .model_provider(provider, model);
    if let Some(role_provider) = context_compaction {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    if let Some(role_provider) = approval_review {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }

    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(root, &[]),
        false,
        None,
    )?)
    .map_err(unexpected)?
    .with_cli_bwrap_permissioned_process_runner(
        admission,
        runner,
        permissioned_process_runner_factory,
    );
    let mut builder = with_workspace_coding_loop_profile(builder, profile)?;
    if let Some(policy) = retry_policy {
        builder = builder.model_retry_policy(policy);
    }
    builder.build().map_err(unexpected)
}

#[cfg(test)]
pub(crate) fn build_scripted_permission_network_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let session_id =
        merry_core::SessionId::new(PERMISSION_NETWORK_SMOKE_SESSION_ID).map_err(unexpected)?;
    let provider = PermissionNetworkSmokeProvider::new()?;
    let review_provider = PermissionNetworkSmokeReviewProvider::new()?;
    let builder = Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction)
        .model_provider(
            Arc::new(provider),
            ModelName::new("merry-permission-network-smoke-scripted").map_err(unexpected)?,
        )
        .model_provider_for_role(
            RuntimeModelRole::ApprovalReview,
            Arc::new(review_provider),
            ModelName::new("merry-permission-network-smoke-review-scripted").map_err(unexpected)?,
        );

    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(root, &[]),
        false,
        None,
    )?)
    .map_err(unexpected)?
    .with_cli_bwrap_permissioned_process_runner(
        admission,
        runner,
        permissioned_process_runner_factory,
    );
    with_workspace_coding_loop_profile(builder, profile)?
        .build()
        .map_err(unexpected)
}

pub(crate) struct CodingLoopLiveRuntimeOptions {
    pub(crate) automatic_compaction: AutomaticCompactionConfig,
    pub(crate) skill_roots: Vec<PathBuf>,
    pub(crate) subagents: crate::coding_runtime::CodingSubagentsConfig,
}

pub(crate) fn coding_loop_subagent_live_smoke_config()
-> Result<crate::coding_runtime::CodingSubagentsConfig, CliError> {
    Ok(crate::coding_runtime::CodingSubagentsConfig::enabled(
        merry_runtime::SubagentConfig::new(2, 1).map_err(unexpected)?,
    ))
}

fn task_smoke_workspace_tool_limits() -> WorkspaceToolLimits {
    WorkspaceToolLimits {
        max_patch_bytes: super::CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES,
        ..WorkspaceToolLimits::default()
    }
}
