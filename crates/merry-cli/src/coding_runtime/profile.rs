use super::CodingRuntimeError;
use merry_runtime::{RuntimeBuilder, RuntimeProfile};
use merry_tool_workspace::{
    WorkspaceCodingLoopProfile, WorkspaceRuntimeProfileBuilderExt, WorkspaceToolLimits,
    WorkspaceToolsConfig,
};
use std::path::{Path, PathBuf};

pub(crate) fn with_workspace_coding_loop_profile(
    builder: RuntimeBuilder,
    profile: WorkspaceCodingLoopProfile,
) -> Result<RuntimeBuilder, CodingRuntimeError> {
    let profile = RuntimeProfile::builder()
        .with_workspace_coding_loop(profile)
        .map_err(CodingRuntimeError::from)?
        .build()
        .map_err(CodingRuntimeError::from)?;
    builder
        .with_profile(profile)
        .map_err(|source| CodingRuntimeError::RuntimeProfileApply { source })
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
    let _ = skill_roots;
    vec![root.to_path_buf()]
}

pub(crate) fn workspace_tools_config(
    roots: Vec<PathBuf>,
    allow_hidden_workspace_paths: bool,
    limits_override: Option<WorkspaceToolLimits>,
) -> Result<WorkspaceToolsConfig, CodingRuntimeError> {
    let limits = limits_override.unwrap_or_default();
    Ok(WorkspaceToolsConfig::new(roots)
        .with_allow_hidden(allow_hidden_workspace_paths)
        .with_limits(limits))
}

pub(crate) fn workspace_tools_config_with_resources(
    roots: Vec<PathBuf>,
    readonly_resource_roots: Vec<PathBuf>,
    allow_hidden_workspace_paths: bool,
    limits_override: Option<WorkspaceToolLimits>,
) -> Result<WorkspaceToolsConfig, CodingRuntimeError> {
    Ok(WorkspaceToolsConfig::new(roots)
        .with_readonly_resource_roots(readonly_resource_roots)
        .with_allow_hidden(allow_hidden_workspace_paths)
        .with_limits(limits_override.unwrap_or_default()))
}
