use crate::cli_error::{CliError, unexpected};
use crate::debug::coding_loop::CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES;
use merry_runtime::{RuntimeBuilder, RuntimeProfile};
use merry_tool_workspace::{
    WorkspaceCodingLoopProfile, WorkspaceRuntimeProfileBuilderExt, WorkspaceToolLimits,
    WorkspaceToolsConfig,
};
use std::path::{Path, PathBuf};

pub(crate) fn with_workspace_coding_loop_profile(
    builder: RuntimeBuilder,
    profile: WorkspaceCodingLoopProfile,
) -> Result<RuntimeBuilder, CliError> {
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .map_err(unexpected)?
        .build()
        .map_err(unexpected)?;
    builder.with_profile(profile).map_err(unexpected)
}

pub(super) fn with_workspace_coding_loop_profile_for_child(
    builder: RuntimeBuilder,
    profile: WorkspaceCodingLoopProfile,
) -> Result<RuntimeBuilder, merry_runtime::RuntimeError> {
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
            reason: "child workspace coding loop profile application failed",
        })?
        .build()
        .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
            reason: "child runtime profile build failed",
        })?;
    builder.with_profile(profile)
}

pub(crate) fn coding_loop_workspace_roots(root: &Path, skill_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = vec![root.to_path_buf()];
    roots.extend(skill_roots.iter().filter(|root| root.is_dir()).cloned());
    roots
}

pub(crate) fn workspace_tools_config(
    roots: Vec<PathBuf>,
    allow_hidden_workspace_paths: bool,
    task_smoke_patch_limit: bool,
    max_patch_bytes_override: Option<usize>,
) -> Result<WorkspaceToolsConfig, CliError> {
    let max_patch_bytes = max_patch_bytes_override.unwrap_or_else(|| {
        if task_smoke_patch_limit {
            CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES
        } else {
            WorkspaceToolLimits::default().max_patch_bytes
        }
    });
    Ok(WorkspaceToolsConfig::new(roots)
        .with_allow_hidden(allow_hidden_workspace_paths)
        .with_limits(WorkspaceToolLimits {
            max_patch_bytes,
            ..WorkspaceToolLimits::default()
        }))
}
