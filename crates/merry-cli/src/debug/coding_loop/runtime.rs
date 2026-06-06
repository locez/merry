use crate::cli_error::{CliError, debug_openai_usage_error, unexpected};
use crate::coding_runtime::{
    CodingLoopRuntimeOptions, build_coding_loop_runtime, coding_loop_workspace_roots,
    with_workspace_coding_loop_profile, workspace_tools_config,
};
use crate::config;
use crate::provider_config::{
    OpenAiRuntimeConfig, openai_approval_review_provider, openai_context_compaction_provider,
};
use merry_llm::ModelName;
use merry_provider_openai::OpenAiProvider;
#[cfg(test)]
use merry_runtime::RuntimeModelRole;
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AutomaticCompactionConfig,
    PermissionedProcessRunnerFactory, ProcessRunner, Runtime,
};
use merry_tool_workspace::WorkspaceCodingLoopProfile;
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
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let provider = CodingLoopSmokeProvider::new(relative_cwd)?;
    build_coding_loop_runtime(
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
            permissioned_process_runner_factory,
            skill_roots: Vec::new(),
            subagents: config::SubagentsConfig::default(),
        },
    )
}

pub(crate) fn build_coding_loop_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let provider = OpenAiProvider::new(config.primary.provider);
    let context_compaction = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?;
    let approval_review = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?;
    build_coding_loop_runtime(
        CODING_LOOP_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: true,
            approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: config.retry_policy,
            context_compaction,
            permissioned_process_runner_factory,
            skill_roots: options.skill_roots,
            subagents: options.subagents,
        },
    )
}

pub(crate) fn build_coding_loop_task_smoke_runtime(
    root: &Path,
    relative_cwd: Option<&str>,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    fixture: CodingLoopTaskSmokeFixture,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let provider = CodingLoopTaskSmokeProvider::new(relative_cwd, fixture)?;
    build_coding_loop_runtime(
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
            permissioned_process_runner_factory,
            skill_roots: Vec::new(),
            subagents: config::SubagentsConfig::default(),
        },
    )
}

pub(crate) fn build_coding_loop_task_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let provider = OpenAiProvider::new(config.primary.provider);
    let context_compaction = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?;
    let approval_review = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?;
    build_coding_loop_runtime(
        CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: true,
            approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: config.retry_policy,
            context_compaction,
            permissioned_process_runner_factory,
            skill_roots: options.skill_roots,
            subagents: options.subagents,
        },
    )
}

pub(crate) fn build_coding_loop_subagent_live_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    options: CodingLoopLiveRuntimeOptions,
) -> Result<Runtime, CliError> {
    let provider = OpenAiProvider::new(config.primary.provider);
    let context_compaction = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?;
    let approval_review = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?;
    build_coding_loop_runtime(
        CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID,
        root,
        admission,
        Arc::new(provider),
        ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            approval_review,
            automatic_compaction: options.automatic_compaction,
            retry_policy: config.retry_policy,
            context_compaction,
            permissioned_process_runner_factory,
            skill_roots: options.skill_roots,
            subagents: options.subagents,
        },
    )
}

pub(crate) fn build_permission_network_smoke_runtime(
    root: &Path,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    config: OpenAiRuntimeConfig,
    runner: Arc<dyn ProcessRunner>,
    permissioned_process_runner_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    automatic_compaction: AutomaticCompactionConfig,
) -> Result<Runtime, CliError> {
    let session_id =
        merry_core::SessionId::new(PERMISSION_NETWORK_SMOKE_SESSION_ID).map_err(unexpected)?;
    let provider = OpenAiProvider::new(config.primary.provider);
    let mut builder = Runtime::builder(session_id)
        .automatic_compaction(automatic_compaction)
        .model_provider(
            Arc::new(provider),
            ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?,
        );
    if let Some(role_provider) = config
        .context_compaction
        .map(openai_context_compaction_provider)
        .transpose()?
    {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    if let Some(role_provider) = config
        .approval_review
        .map(openai_approval_review_provider)
        .transpose()?
    {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }

    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(root, &[]),
        false,
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
    if let Some(policy) = config.retry_policy {
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
    pub(crate) subagents: config::SubagentsConfig,
}

pub(crate) fn coding_loop_subagent_live_smoke_config() -> Result<config::SubagentsConfig, CliError>
{
    Ok(config::SubagentsConfig::enabled(
        merry_runtime::SubagentConfig::new(2, 1).map_err(unexpected)?,
    ))
}
