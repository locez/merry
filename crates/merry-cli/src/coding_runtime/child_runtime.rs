use super::process::ActionProcessBackend;
use super::profile::{
    coding_loop_workspace_roots, with_workspace_coding_loop_profile_for_child,
    workspace_tools_config,
};
use merry_llm::{ModelName, ModelProvider};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, ChildRuntimeFactory, ChildRuntimeInput,
    PermissionedProcessRunnerFactory, ProcessRunner, Runtime,
};
use merry_tool_workspace::{
    CODING_LOOP_PROCESS_TOOL, WORKSPACE_PATCH_TOOL, WorkspaceCodingLoopProfile,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub(crate) struct CodingLoopChildRuntimeFactory {
    root: PathBuf,
    admission: AcceptedLocalWorkspaceProcessAdmission,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    runner: Arc<dyn ProcessRunner>,
    permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    skill_roots: Vec<PathBuf>,
    allow_hidden_workspace_paths: bool,
}

impl CodingLoopChildRuntimeFactory {
    pub(crate) fn new(
        root: &Path,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        process_backend: ActionProcessBackend,
        skill_roots: Vec<PathBuf>,
        allow_hidden_workspace_paths: bool,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            admission,
            provider,
            model,
            runner: process_backend.runner(),
            permissioned_factory: process_backend.permissioned_factory(),
            skill_roots,
            allow_hidden_workspace_paths,
        }
    }
}

impl ChildRuntimeFactory for CodingLoopChildRuntimeFactory {
    fn build_child(
        &self,
        input: ChildRuntimeInput,
    ) -> Result<Runtime, merry_runtime::RuntimeError> {
        let allow_patch = input.allowed_tools.is_empty()
            || input
                .allowed_tools
                .iter()
                .any(|tool| tool.as_str() == WORKSPACE_PATCH_TOOL);
        let allow_local_workspace_process = input.allowed_tools.is_empty()
            || input
                .allowed_tools
                .iter()
                .any(|tool| tool.as_str() == CODING_LOOP_PROCESS_TOOL);
        let builder = Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(Arc::clone(&self.provider), self.model.clone());
        let mut profile = WorkspaceCodingLoopProfile::new(
            workspace_tools_config(
                coding_loop_workspace_roots(&self.root, &self.skill_roots),
                self.allow_hidden_workspace_paths,
                false,
                None,
            )
            .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
                reason: "child workspace tool config was invalid",
            })?,
        )
        .map_err(|_| merry_runtime::RuntimeError::InvalidStepInput {
            reason: "child workspace coding loop profile was invalid",
        })?;
        if allow_patch {
            profile = profile.with_patch_tool();
        }
        profile = if allow_local_workspace_process {
            profile.with_cli_bwrap_permissioned_process_runner(
                self.admission,
                Arc::clone(&self.runner),
                Arc::clone(&self.permissioned_factory),
            )
        } else {
            profile.with_read_only_process_runner(Arc::clone(&self.runner))
        };
        with_workspace_coding_loop_profile_for_child(builder, profile)?.build()
    }
}
