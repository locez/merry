use crate::CodingAgentProfileBuilder;
use merry_llm::{ModelName, ModelProvider};
use merry_process::ProcessBackend;
use merry_runtime::{
    AutomaticCompactionConfig, ChildRuntimeFactory, ChildRuntimeInput, ChildWorkspaceScope,
    Runtime, RuntimeError, SubagentConfig, SubagentManager, ToolAdmission,
    subagent_registered_tools,
};
use merry_tool_workspace::CODING_LOOP_PROCESS_TOOL;
use std::sync::Arc;

/// Coding-owned child runtime factory.
///
/// This factory combines the shared coding profile with runtime-owned child
/// scope, subagent, admission, and plan-link state. It deliberately accepts a
/// host-process backend contract instead of a CLI or sandbox type.
#[derive(Clone)]
pub struct CodingChildRuntimeFactory {
    profile_builder: CodingAgentProfileBuilder,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    process_backend: Arc<dyn ProcessBackend>,
    subagent_config: SubagentConfig,
    automatic_compaction: AutomaticCompactionConfig,
}

impl CodingChildRuntimeFactory {
    /// Creates a coding child-runtime factory from explicit composition inputs.
    #[allow(
        clippy::too_many_arguments,
        reason = "child runtime dependencies remain explicit at the composition boundary"
    )]
    pub fn new(
        profile_builder: CodingAgentProfileBuilder,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        process_backend: Arc<dyn ProcessBackend>,
        subagent_config: SubagentConfig,
        automatic_compaction: AutomaticCompactionConfig,
    ) -> Self {
        Self {
            profile_builder,
            provider,
            model,
            process_backend,
            subagent_config,
            automatic_compaction,
        }
    }
}

impl ChildRuntimeFactory for CodingChildRuntimeFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let process_session = self.process_backend.new_session();
        let runner = process_session.runner();
        let allow_local_workspace_process = input
            .allowed_tools
            .iter()
            .any(|tool| tool.as_str() == CODING_LOOP_PROCESS_TOOL);
        let mut builder = Runtime::builder(input.session_id.clone())
            .task_anchor(input.task_anchor)
            .automatic_compaction(self.automatic_compaction)
            .model_provider(Arc::clone(&self.provider), self.model.clone())
            .tool_admission(ToolAdmission::allow_only(input.allowed_tools.clone()));
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
        let [spawn_tool, wait_tool, cancel_tool] =
            subagent_registered_tools(child_manager.clone()).map_err(RuntimeError::from)?;
        builder = builder
            .subagent_parent_scope(input.workspace_scope.clone())
            .subagent_manager(child_manager);
        if let Some(runtime) = parent_plan_link_runtime {
            builder = builder.subagent_parent_plan_link_runtime(runtime);
        }
        if let Some(control) = input.plan_subagent_control {
            builder = builder.plan_subagent_control(control);
        }
        if let Some(scope) = input.plan_subagent_scope {
            builder = builder.plan_subagent_scope(scope);
        }
        let write_scope_is_explicit = input.task.write_scope_is_explicit();
        let workspace_scope = input.workspace_scope;
        let has_child_workspace_boundary =
            child_has_workspace_boundary(&workspace_scope, write_scope_is_explicit);
        let mut profile = self
            .profile_builder
            .clone()
            .patch_write_scope(workspace_scope.write_scope().to_vec())
            .forbidden_paths(workspace_scope.forbidden_paths().to_vec())
            .register_tools([spawn_tool, wait_tool, cancel_tool]);
        profile = if allow_local_workspace_process && !has_child_workspace_boundary {
            profile.accepted_process_session(process_session)
        } else {
            profile.read_only_process_runner(runner)
        };
        let profile = profile
            .build()
            .map_err(|source| RuntimeError::ChildRuntimeBuild {
                message: source.to_string(),
            })?;
        profile.apply_to(builder)?.build()
    }
}

fn child_has_workspace_boundary(
    workspace_scope: &ChildWorkspaceScope,
    write_scope_is_explicit: bool,
) -> bool {
    // RuntimeBuilder applies parent capabilities before ChildRuntimeFactory
    // construction, so manager-spawned tasks always carry explicit scopes.
    // Keep the flag for direct factory composition that bypasses the manager.
    write_scope_is_explicit
        || !workspace_scope.write_scope().is_empty()
        || !workspace_scope.forbidden_paths().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_runtime::SubagentTaskSpec;

    #[test]
    fn explicit_empty_write_scope_is_a_child_workspace_boundary() {
        let task = SubagentTaskSpec::new("Read the assigned files.", 1)
            .expect("valid task")
            .with_write_scope(Vec::<&str>::new())
            .expect("empty write scope is valid");
        let scope = ChildWorkspaceScope::from_task(&task);

        assert!(child_has_workspace_boundary(
            &scope,
            task.write_scope_is_explicit()
        ));
    }
}
