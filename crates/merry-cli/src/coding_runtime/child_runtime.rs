use super::process::ActionProcessBackend;
use super::profile::{
    coding_loop_workspace_roots, with_workspace_coding_loop_profile_for_child,
    workspace_tools_config,
};
use merry_llm::{ModelName, ModelProvider};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, ChildRuntimeFactory, ChildRuntimeInput,
    PermissionedProcessRunnerFactory, ProcessRunner, ProjectRules, Runtime, SubagentConfig,
    SubagentManager, subagent_registered_tools,
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
    project_rules: Option<ProjectRules>,
    skill_roots: Vec<PathBuf>,
    allow_hidden_workspace_paths: bool,
    subagent_config: SubagentConfig,
}

impl CodingLoopChildRuntimeFactory {
    #[allow(
        clippy::too_many_arguments,
        reason = "child runtime dependencies remain explicit at the single construction boundary"
    )]
    pub(crate) fn new(
        root: &Path,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        process_backend: ActionProcessBackend,
        project_rules: Option<ProjectRules>,
        skill_roots: Vec<PathBuf>,
        allow_hidden_workspace_paths: bool,
        subagent_config: SubagentConfig,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            admission,
            provider,
            model,
            runner: process_backend.runner(),
            permissioned_factory: process_backend.permissioned_factory(),
            project_rules,
            skill_roots,
            allow_hidden_workspace_paths,
            subagent_config,
        }
    }
}

impl ChildRuntimeFactory for CodingLoopChildRuntimeFactory {
    fn build_child(
        &self,
        input: ChildRuntimeInput,
    ) -> Result<Runtime, merry_runtime::RuntimeError> {
        let allow_patch = input
            .allowed_tools
            .iter()
            .any(|tool| tool.as_str() == WORKSPACE_PATCH_TOOL);
        let allow_local_workspace_process = input
            .allowed_tools
            .iter()
            .any(|tool| tool.as_str() == CODING_LOOP_PROCESS_TOOL);
        let mut builder = Runtime::builder(input.session_id.clone())
            .task_anchor(input.task_anchor)
            .model_provider(Arc::clone(&self.provider), self.model.clone())
            .registered_tool_allowlist(input.allowed_tools.clone());
        if let Some(activity_hub) = input.activity_hub.clone() {
            builder = builder.subagent_activity_hub(activity_hub);
        }
        let parent_plan_link_runtime = input.plan_link_runtime.clone();
        let child_factory: Arc<dyn ChildRuntimeFactory> = Arc::new(self.clone());
        let child_manager = SubagentManager::runtime_controlled_at_depth(
            input.session_id.clone(),
            self.subagent_config,
            child_factory,
            input
                .allowed_tools
                .iter()
                .any(|tool| tool.as_str() == "spawn_subagents"),
            input.depth,
        );
        let [spawn_tool, wait_tool, cancel_tool] = subagent_registered_tools(child_manager.clone())
            .map_err(merry_runtime::RuntimeError::from)?;
        builder = builder
            .subagent_parent_scope(input.workspace_scope.clone())
            .subagent_manager(child_manager)
            .register_tool(spawn_tool)
            .register_tool(wait_tool)
            .register_tool(cancel_tool);
        if let Some(runtime) = parent_plan_link_runtime {
            builder = builder.subagent_parent_plan_link_runtime(runtime);
        }
        if let Some(control) = input.plan_subagent_control {
            builder = builder.plan_subagent_control(control);
        }
        if let Some(scope) = input.plan_subagent_scope {
            builder = builder.plan_subagent_scope(scope);
        }
        if let Some(project_rules) = self.project_rules.clone() {
            builder = builder.project_rules(project_rules);
        }
        let workspace_scope = input.workspace_scope;
        let has_child_workspace_boundary = !workspace_scope.write_scope().is_empty()
            || !workspace_scope.forbidden_paths().is_empty();
        let mut profile = WorkspaceCodingLoopProfile::new(
            workspace_tools_config(
                coding_loop_workspace_roots(&self.root, &self.skill_roots),
                self.allow_hidden_workspace_paths,
                None,
            )
            .map(|config| {
                config
                    .with_patch_write_scope(Some(workspace_scope.write_scope().to_vec()))
                    .with_forbidden_paths(workspace_scope.forbidden_paths().to_vec())
            })
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
        profile = if allow_local_workspace_process && !has_child_workspace_boundary {
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
