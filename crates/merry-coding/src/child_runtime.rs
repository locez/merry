use crate::{
    CODING_LOOP_PROCESS_TOOL, CodingAgentProfileBuilder, SubagentResolutionInput,
    SubagentRoleCatalog, WorktreeIsolationManager, coding_subagent_policy_prompt,
    resolve_subagent_runtime, runtime::CodingRuntimePolicy,
};
use merry_core::{SessionId, ToolName};
use merry_llm::{ModelName, ModelProvider};
use merry_process::{ProcessBackend, ProcessSessionRequest};
use merry_runtime::{
    BuiltChildRuntime, ChildRuntimeFactory, ChildRuntimeInput, CompactionConfig,
    LocalWorkspaceProcessSandboxProfile, ProcessSessionPermissionView, PromptBlock, PromptProfile,
    Runtime, RuntimeError, SUBAGENT_HANDOFF_CONTEXT_ID, SubagentConfig, SubagentManager,
    SubagentWorkspaceMode, ToolAdmission, preload_context_files, render_subagent_handoff,
    subagent_registered_tools_with_roles,
};
use std::{num::NonZeroUsize, path::Path, sync::Arc};

/// Coding-owned child runtime factory.
///
/// This factory combines the shared coding profile with runtime-owned child
/// scope, subagent, and admission state. It deliberately accepts a
/// host-process backend contract instead of a CLI or sandbox type.
#[derive(Clone)]
pub(crate) struct CodingChildRuntimeFactory {
    composition: CodingRuntimeComposition,
}

/// Normalized parent/child inputs shared by coding runtime composition.
///
/// The parent builder creates this value once after selecting provider, process,
/// subagent, and compaction policy. Child runtimes reuse the same composition
/// instead of receiving a second set of independently assembled dependencies.
#[derive(Clone)]
pub(crate) struct CodingRuntimeComposition {
    pub(crate) profile_builder: CodingAgentProfileBuilder,
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
    pub(crate) process_backend: Arc<dyn ProcessBackend>,
    pub(crate) parent_process_permissions: Option<ProcessSessionPermissionView>,
    pub(crate) parent_workspace_writable: bool,
    pub(crate) subagent_config: SubagentConfig,
    pub(crate) role_catalog: SubagentRoleCatalog,
    pub(crate) automatic_compaction: CompactionConfig,
    pub(crate) max_parallel_tool_calls: Option<NonZeroUsize>,
    pub(crate) policy: CodingRuntimePolicy,
    pub(crate) worktree_manager: WorktreeIsolationManager,
}

impl CodingChildRuntimeFactory {
    pub(crate) fn from_composition(composition: CodingRuntimeComposition) -> Self {
        Self { composition }
    }
}

impl ChildRuntimeFactory for CodingChildRuntimeFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        self.build_child_with_overrides(input)
            .map(|built| built.runtime)
    }

    fn build_child_with_overrides(
        &self,
        input: ChildRuntimeInput,
    ) -> Result<BuiltChildRuntime, RuntimeError> {
        let workspace_scope = input.workspace_scope;
        let resolved = resolve_subagent_runtime(
            &self.composition.role_catalog,
            &SubagentResolutionInput {
                task: &input.task,
                parent_allowed_tools: &input.allowed_tools,
                workspace_scope: &workspace_scope,
                parent_workspace_writable: self.composition.parent_workspace_writable,
                depth: input.depth,
                max_depth: self.composition.subagent_config.max_depth(),
                max_model_turns_cap: self.composition.subagent_config.max_model_turns(),
                parent_primary_model: input.parent_primary_model.as_ref(),
                fallback_provider: Arc::clone(&self.composition.provider),
                fallback_model: &self.composition.model,
                parent_generation_config: &input.parent_generation_config,
            },
        )
        .map_err(|source| RuntimeError::ChildRuntimeBuild {
            message: source.to_string(),
        })?;

        let child_workspace_writable = resolved.workspace_writable;
        let prepared_worktree = if input.task.workspace_mode() == SubagentWorkspaceMode::Worktree {
            let agent_label = input
                .agent_id
                .as_ref()
                .map(|id| id.as_str())
                .unwrap_or(input.session_id.as_str());
            let prepared = self
                .composition
                .worktree_manager
                .prepare_or_reattach(
                    self.composition.profile_builder.workspace_root(),
                    &input.parent_session_id,
                    agent_label,
                    input.task.worktree_base(),
                    input.existing_worktree.as_ref(),
                )
                .map_err(|source| RuntimeError::ChildRuntimeBuild {
                    message: source.to_string(),
                })?;
            Some(prepared)
        } else {
            None
        };
        let effective_workspace_root = prepared_worktree
            .as_ref()
            .map(|wt| wt.child_workspace_root.as_path())
            .unwrap_or_else(|| self.composition.profile_builder.workspace_root());
        let mut session_request =
            ProcessSessionRequest::new().with_workspace_writable(child_workspace_writable);
        if let Some(wt) = prepared_worktree.as_ref() {
            session_request = session_request
                .with_workspace_root(&wt.child_workspace_root)
                .with_extra_path_rules(wt.sandbox_path_rules(child_workspace_writable));
        }
        if let Some(view) = self.composition.parent_process_permissions.clone() {
            session_request = session_request.with_inherited_permissions(view);
        }
        let process_session = self
            .composition
            .process_backend
            .new_session_with(session_request)
            .map_err(|source| RuntimeError::ChildRuntimeBuild {
                message: source.to_string(),
            })?;
        let runner = process_session.runner();
        let allow_local_workspace_process = resolved
            .allowed_tools
            .iter()
            .any(|tool| tool.as_str() == CODING_LOOP_PROCESS_TOOL);

        let cache_key =
            shared_child_prompt_cache_key(&input.parent_session_id, &resolved.role_name);
        // Do not install `task_anchor` redundantly: the full delegated task is
        // passed directly as the child's first User message.
        let mut builder = Runtime::builder(input.session_id.clone())
            .prompt_cache_key(cache_key)
            .automatic_compaction(self.composition.automatic_compaction.clone())
            .model_provider(Arc::clone(&resolved.provider), resolved.model.clone())
            .tool_admission(ToolAdmission::allow_only(resolved.allowed_tools.clone()));
        if let Some(retry_policy) = resolved.retry_policy {
            builder = builder.model_retry_policy(retry_policy);
        }
        if let Some(limit) = self.composition.max_parallel_tool_calls {
            builder = builder.max_parallel_tool_calls(limit);
        }
        if let Some(activity_hub) = input.activity_hub.clone() {
            builder = builder.subagent_activity_hub(activity_hub);
        }
        if let Some(store) = input.session_store {
            builder = builder.session_store(store);
        }
        if let Some(loaded) = input.loaded_session.and_then(|resumable| resumable.take()) {
            builder = builder.with_loaded_session(loaded);
        }
        if let Some(agent_id) = input.agent_id {
            builder = builder.permission_actor(merry_runtime::PermissionActor::Subagent {
                agent_id,
                display_name: input.task.display_name().map(str::to_owned),
                role: Some(resolved.role_name.clone()),
            });
        }
        let preloaded_files = preload_context_files(
            effective_workspace_root,
            &workspace_scope,
            input.task.context_files(),
        );
        if let Some(handoff) =
            render_subagent_handoff(input.task.expected_output(), &preloaded_files)
        {
            builder = builder.initial_context_summary(SUBAGENT_HANDOFF_CONTEXT_ID, &handoff);
        }
        let child_factory: Arc<dyn ChildRuntimeFactory> = Arc::new(self.clone());
        let child_manager = SubagentManager::runtime_controlled_at_depth(
            input.session_id.clone(),
            self.composition.subagent_config,
            child_factory,
            resolved
                .allowed_tools
                .iter()
                .any(|tool| tool.as_str() == "spawn_subagents"),
            input.depth,
        );
        let role_summaries = self.composition.role_catalog.enabled_role_summaries();
        let [spawn_tool, send_tool, wait_tool, cancel_tool] =
            subagent_registered_tools_with_roles(child_manager.clone(), &role_summaries)
                .map_err(RuntimeError::from)?;
        builder = builder
            .subagent_parent_scope(workspace_scope.clone())
            .subagent_manager(child_manager);
        let child_prompt_profile = build_child_prompt_profile(
            &resolved.allowed_tools,
            resolved.role_instructions.as_deref(),
        )
        .map_err(|source| RuntimeError::ChildRuntimeBuild {
            message: source.to_string(),
        })?;
        let memory = self.composition.profile_builder.durable_memory_handle();
        let effective_write_scope = if child_workspace_writable {
            workspace_scope.write_scope().to_vec()
        } else {
            Vec::new()
        };
        let mut profile = self
            .composition
            .profile_builder
            .clone()
            .root(effective_workspace_root)
            .prompt_profile(child_prompt_profile)
            .patch_write_scope(effective_write_scope)
            .forbidden_paths(workspace_scope.forbidden_paths().to_vec())
            .register_tools([spawn_tool, send_tool, wait_tool, cancel_tool]);
        if !workspace_scope
            .read_scope()
            .iter()
            .any(|path| path == Path::new("."))
        {
            profile = profile.read_scope(workspace_scope.read_scope().to_vec());
        }
        let use_accepted_session = allow_local_workspace_process
            && (child_workspace_writable
                || process_session.admission().sandbox_profile()
                    == LocalWorkspaceProcessSandboxProfile::LocalWorkspace);
        profile = if use_accepted_session {
            profile.accepted_process_session(process_session)
        } else {
            profile.read_only_process_runner(runner)
        };
        let profile = profile
            .build()
            .map_err(|source| RuntimeError::ChildRuntimeBuild {
                message: source.to_string(),
            })?;
        let mut builder = profile.apply_to(builder)?;
        if let Some(memory) = memory {
            builder = builder.durable_memory(memory, false);
        }
        let child_policy = if prepared_worktree.is_some() {
            self.composition
                .policy
                .clone()
                .with_permission_review_workspace_root(effective_workspace_root)
        } else {
            self.composition.policy.clone()
        };
        let runtime = child_policy.apply_to(builder).build()?;
        Ok(BuiltChildRuntime {
            runtime,
            generation_config: Some(resolved.generation_config),
            max_model_turns: Some(resolved.max_model_turns),
            timeout_secs: resolved.timeout_secs,
            max_total_tokens: resolved.max_total_tokens,
            resolved_role: Some(resolved.role_name),
            workspace_lifecycle: prepared_worktree.map(|wt| wt.lifecycle),
        })
    }
}

fn build_child_prompt_profile(
    allowed_tools: &[ToolName],
    role_instructions: Option<&str>,
) -> Result<PromptProfile, merry_runtime::PromptError> {
    let mut profile =
        PromptProfile::for_subagent(allowed_tools).with_stable_block(PromptBlock::new(
            "merry_coding_subagent_policy",
            coding_subagent_policy_prompt(allowed_tools),
        )?)?;
    if let Some(role_text) = role_instructions.filter(|text| !text.trim().is_empty()) {
        profile = profile.with_stable_block(PromptBlock::new("merry_subagent_role", role_text)?)?;
    }
    Ok(profile)
}

fn shared_child_prompt_cache_key(parent_session_id: &SessionId, effective_role: &str) -> SessionId {
    let sanitized_role: String = effective_role
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(32)
        .collect();
    let suffix = if sanitized_role.is_empty() {
        "-role-default".to_owned()
    } else {
        format!("-role-{sanitized_role}")
    };
    let max_parent_chars = 128usize.saturating_sub(suffix.len());
    let parent_prefix: String = parent_session_id
        .as_str()
        .chars()
        .take(max_parent_chars)
        .collect();
    SessionId::new(&format!("{parent_prefix}{suffix}"))
        .unwrap_or_else(|_| parent_session_id.clone())
}

#[cfg(test)]
mod tests;
