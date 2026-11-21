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
/// scope, subagent, admission, and plan-link state. It deliberately accepts a
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
        let parent_plan_link_runtime = input.plan_link_runtime.clone();
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
        if let Some(runtime) = parent_plan_link_runtime {
            builder = builder.subagent_parent_plan_link_runtime(runtime);
        }
        if let Some(control) = input.plan_subagent_control {
            builder = builder.plan_subagent_control(control);
        }
        if let Some(scope) = input.plan_subagent_scope {
            builder = builder.plan_subagent_scope(scope);
        }
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
mod tests {
    use super::*;
    use crate::{CodingAgentProfileBuilder, CodingModelRoleConfig, CodingPermissionPolicy};
    use futures_util::StreamExt;
    use merry_core::{
        RuntimeJournalEvent, RuntimeJournalPayload, SessionId, ToolCallResultStatus, ToolName,
    };
    use merry_llm::{
        FinishReason, GenerationConfig, ModelEvent, ModelName, ModelOutput, ModelProvider,
        ModelResponse, ModelRetryPolicy, ModelToolCall, ModelToolCallId, ToolArguments,
        testing::FakeModelProvider,
    };
    use merry_process::{LocalProcessBackend, ProcessBackend, ProcessSession, TokioProcessRunner};
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, ChildWorkspaceScope, CitationCompactionPolicy,
        CompactionConfig, PermissionAdmissionContext, PermissionAdmissionDecision,
        PermissionAdmissionFuture, PermissionAdmissionSource,
        ProcessRunner as RuntimeProcessRunner, Runtime, RuntimeModelRole,
        StaticPermissionedProcessRunnerFactory, StepContext, StepInput, SubagentConfig,
        SubagentTaskSpec, TaskAnchor,
    };
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn explicit_empty_write_scope_requests_read_only_child_workspace() {
        let catalog = SubagentRoleCatalog::builtin();
        let fallback_provider: Arc<dyn ModelProvider> =
            Arc::new(FakeModelProvider::new(Vec::new()));
        let fallback_model = ModelName::new("fallback").expect("valid model");
        let generation = GenerationConfig::default();

        let task = SubagentTaskSpec::new("Read the assigned files.", 1)
            .expect("valid task")
            .with_write_scope(Vec::<&str>::new())
            .expect("empty write scope is valid");
        let scope = ChildWorkspaceScope::from_task(&task);
        let ro = resolve_subagent_runtime(
            &catalog,
            &SubagentResolutionInput {
                task: &task,
                parent_allowed_tools: &[],
                workspace_scope: &scope,
                parent_workspace_writable: true,
                depth: 1,
                max_depth: 1,
                max_model_turns_cap: 80,
                parent_primary_model: None,
                fallback_provider: Arc::clone(&fallback_provider),
                fallback_model: &fallback_model,
                parent_generation_config: &generation,
            },
        )
        .expect("ro resolves");
        assert!(!ro.workspace_writable);

        let writable_task = SubagentTaskSpec::new("Edit assigned files.", 1)
            .expect("valid task")
            .with_write_scope(["src"])
            .expect("write scope is valid");
        let writable_scope = ChildWorkspaceScope::from_task(&writable_task);
        let rw = resolve_subagent_runtime(
            &catalog,
            &SubagentResolutionInput {
                task: &writable_task,
                parent_allowed_tools: &[],
                workspace_scope: &writable_scope,
                parent_workspace_writable: true,
                depth: 1,
                max_depth: 1,
                max_model_turns_cap: 80,
                parent_primary_model: None,
                fallback_provider,
                fallback_model: &fallback_model,
                parent_generation_config: &generation,
            },
        )
        .expect("rw resolves");
        assert!(rw.workspace_writable);
    }

    fn process_backend() -> Arc<dyn ProcessBackend> {
        let runner: Arc<dyn RuntimeProcessRunner> = Arc::new(TokioProcessRunner::new());
        let permissioned_factory = Arc::new(StaticPermissionedProcessRunnerFactory::new(
            Arc::clone(&runner),
        ));
        let session = ProcessSession::from_parts(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
            permissioned_factory,
        );
        Arc::new(LocalProcessBackend::from_session(session))
    }

    fn build_factory(
        root: &std::path::Path,
        provider: Arc<dyn ModelProvider>,
        policy: CodingRuntimePolicy,
    ) -> CodingChildRuntimeFactory {
        let profile_builder = CodingAgentProfileBuilder::new(root)
            .patch_tool()
            .retry_policy(ModelRetryPolicy::disabled())
            .accepted_process_session({
                let runner: Arc<dyn RuntimeProcessRunner> = Arc::new(TokioProcessRunner::new());
                let permissioned_factory = Arc::new(StaticPermissionedProcessRunnerFactory::new(
                    Arc::clone(&runner),
                ));
                ProcessSession::from_parts(
                    AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
                    runner,
                    permissioned_factory,
                )
            });
        CodingChildRuntimeFactory::from_composition(CodingRuntimeComposition {
            profile_builder,
            provider,
            model: ModelName::new("child-primary").expect("child model should be valid"),
            process_backend: process_backend(),
            parent_process_permissions: None,
            parent_workspace_writable: true,
            subagent_config: SubagentConfig::new(1, 1)
                .expect("subagent config should be valid")
                .with_model_turn_bounds(1, 80)
                .expect("coding subagent bounds should be valid"),
            role_catalog: SubagentRoleCatalog::builtin(),
            automatic_compaction: CompactionConfig::disabled(),
            max_parallel_tool_calls: None,
            policy,
            worktree_manager: WorktreeIsolationManager::new(),
        })
    }

    fn child_input(session: &str, allowed_tools: Vec<ToolName>) -> ChildRuntimeInput {
        let task = SubagentTaskSpec::new("Exercise the child runtime.", 4)
            .expect("child task should be valid");
        ChildRuntimeInput {
            parent_session_id: SessionId::new("parent-session").expect("parent session valid"),
            agent_id: None,
            session_id: SessionId::new(session).expect("child session should be valid"),
            session_store: None,
            loaded_session: None,
            existing_worktree: None,
            task_anchor: TaskAnchor::new("Exercise the child runtime.")
                .expect("child task anchor should be valid"),
            task,
            allowed_tools,
            workspace_scope: ChildWorkspaceScope::workspace_root(),
            depth: 0,
            generation_config: GenerationConfig::default(),
            parent_primary_model: None,
            parent_generation_config: GenerationConfig::default(),
            plan_subagent_control: None,
            plan_subagent_scope: None,
            plan_link: None,
            plan_link_runtime: None,
            activity_hub: None,
        }
    }

    async fn run_text_step(runtime: &Runtime, text: &str) -> Vec<RuntimeJournalEvent> {
        runtime
            .step(
                StepInput::user_text(text).expect("step input should be valid"),
                StepContext::default(),
            )
            .expect("child step should start")
            .collect()
            .await
    }

    fn completed_text(text: &str) -> ModelEvent {
        ModelEvent::Completed {
            response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
        }
    }

    fn compaction_candidate() -> ModelEvent {
        completed_text(
            r#"{
  "confirmed_decisions": [],
  "rejected_approaches": [],
  "constraints_preferences_boundaries": [],
  "corrected_misunderstandings": [],
  "durable_conclusions": [{"id": "child-c1", "text": "The child runtime compacted its history.", "refs": ["h0"]}],
  "open_questions": [],
  "current_progress_and_next_steps": [],
  "exact_details": [],
  "handoffs": []
}"#,
        )
    }

    fn permission_call_event() -> ModelEvent {
        let call = ModelToolCall::new(
            ModelToolCallId::new("child-permission-call").expect("call id should be valid"),
            ToolName::new("request_permissions").expect("tool name should be valid"),
            ToolArguments::try_from(json!({
                "reason": "Run the exact child command.",
                "requested": {"network": true},
                "for_action": {
                    "command": "printf child-permission",
                    "cwd": "."
                }
            }))
            .expect("permission arguments should be valid"),
        );
        ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        }
    }

    fn approval_event() -> ModelEvent {
        completed_text(
            r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"high","rationale":"The child task authorizes this exact command."}"#,
        )
    }

    fn permission_tool_name() -> ToolName {
        ToolName::new("request_permissions").expect("permission tool name should be valid")
    }

    fn resolved_success(events: &[RuntimeJournalEvent]) -> bool {
        events.iter().any(|event| {
            matches!(
                &event.payload,
                RuntimeJournalPayload::ToolCallResolved { result }
                    if result.status() == ToolCallResultStatus::Succeeded
            )
        })
    }

    #[derive(Clone)]
    struct CountingAdmission {
        calls: Arc<AtomicUsize>,
        decision: PermissionAdmissionDecision,
    }

    impl CountingAdmission {
        fn approving() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                decision: PermissionAdmissionDecision::approved("host approved"),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl PermissionAdmissionSource for CountingAdmission {
        fn review<'a>(
            &'a self,
            _request: merry_runtime::PermissionRequest,
            _context: PermissionAdmissionContext,
        ) -> PermissionAdmissionFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(self.decision.clone())
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_runtime_uses_composed_context_compaction_provider() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let primary = Arc::new(FakeModelProvider::new(vec![
            Ok(completed_text("first child turn")),
            Ok(completed_text("second child turn")),
        ]));
        let compactor = Arc::new(FakeModelProvider::new(vec![Ok(compaction_candidate())]));
        let compaction_role = CodingModelRoleConfig::new(
            RuntimeModelRole::ContextCompaction,
            compactor.clone(),
            ModelName::new("child-compactor").expect("compaction model should be valid"),
        )
        .expect("context compaction role should be valid");
        let policy =
            CodingRuntimePolicy::try_new(vec![compaction_role], CodingPermissionPolicy::default())
                .expect("child policy should not contain duplicate roles");
        let factory = build_factory(temp.path(), primary.clone(), policy);
        let runtime = factory
            .build_child(child_input("child-compaction", Vec::new()))
            .expect("child runtime should build");

        run_text_step(&runtime, "old child history").await;
        run_text_step(&runtime, "retained child history").await;
        let policy = CitationCompactionPolicy::new(None, None, 1)
            .expect("compaction policy should be valid");
        let outcome = runtime
            .compact_context_once(policy, StepContext::default())
            .await
            .expect("child compaction should complete")
            .expect("child history should be compactable");

        assert!(outcome.covered_history_item_count() > 0);
        assert_eq!(compactor.recorded_requests().len(), 1);
        assert_eq!(
            compactor.recorded_requests()[0].model().as_str(),
            "child-compactor"
        );
        assert_eq!(primary.recorded_requests().len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_runtime_uses_composed_approval_review_provider() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let primary = Arc::new(FakeModelProvider::new(vec![Ok(permission_call_event())]));
        let approval = Arc::new(FakeModelProvider::new(vec![Ok(approval_event())]));
        let approval_role = CodingModelRoleConfig::new(
            RuntimeModelRole::ApprovalReview,
            approval.clone(),
            ModelName::new("child-approval").expect("approval model should be valid"),
        )
        .expect("approval role should be valid");
        let policy =
            CodingRuntimePolicy::try_new(vec![approval_role], CodingPermissionPolicy::default())
                .expect("child policy should not contain duplicate roles");
        let factory = build_factory(temp.path(), primary.clone(), policy);
        let runtime = factory
            .build_child(child_input(
                "child-approval-provider",
                vec![permission_tool_name()],
            ))
            .expect("child runtime should build");

        run_text_step(&runtime, "Run the exact child command.").await;
        let pending = runtime.pending_tool_calls().await;
        assert_eq!(pending.len(), 1);
        let resolved = runtime
            .execute_tool_call(
                pending[0].id(),
                merry_runtime::ToolExecutionContext::default(),
            )
            .await
            .expect("child permission request should resolve");

        assert!(resolved_success(&resolved));
        assert_eq!(approval.recorded_requests().len(), 1);
        assert_eq!(
            approval.recorded_requests()[0].model().as_str(),
            "child-approval"
        );
        assert_eq!(primary.recorded_requests().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_runtime_propagates_host_only_and_fully_trusted_permission_modes() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let host_only = CountingAdmission::approving();
        let primary = Arc::new(FakeModelProvider::new(vec![Ok(permission_call_event())]));
        let policy = CodingRuntimePolicy::try_new(
            Vec::new(),
            CodingPermissionPolicy::host_decision_only(Arc::new(host_only.clone())),
        )
        .expect("child policy should not contain duplicate roles");
        let factory = build_factory(temp.path(), primary.clone(), policy);
        let runtime = factory
            .build_child(child_input("child-host-only", vec![permission_tool_name()]))
            .expect("host-only child runtime should build");
        run_text_step(&runtime, "Run through the host admission source.").await;
        let pending = runtime.pending_tool_calls().await;
        let events = runtime
            .execute_tool_call(
                pending[0].id(),
                merry_runtime::ToolExecutionContext::default(),
            )
            .await
            .expect("host-only permission request should resolve");
        assert!(resolved_success(&events));
        assert_eq!(host_only.calls(), 1);

        let fully_trusted_source = CountingAdmission::approving();
        let trusted_primary = Arc::new(FakeModelProvider::new(vec![Ok(permission_call_event())]));
        let trusted_policy =
            CodingRuntimePolicy::try_new(Vec::new(), CodingPermissionPolicy::fully_trusted())
                .expect("child policy should not contain duplicate roles");
        let trusted_factory = build_factory(temp.path(), trusted_primary, trusted_policy);
        let trusted_runtime = trusted_factory
            .build_child(child_input(
                "child-fully-trusted",
                vec![permission_tool_name()],
            ))
            .expect("fully trusted child runtime should build");
        run_text_step(&trusted_runtime, "Run through fully trusted admission.").await;
        let pending = trusted_runtime.pending_tool_calls().await;
        let events = trusted_runtime
            .execute_tool_call(
                pending[0].id(),
                merry_runtime::ToolExecutionContext::default(),
            )
            .await
            .expect("fully trusted permission request should resolve");
        assert!(resolved_success(&events));
        assert_eq!(fully_trusted_source.calls(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_runtime_routes_configured_role_model_and_live_parent_model_switch() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let primary = Arc::new(FakeModelProvider::new(vec![Ok(completed_text(
            "primary reply",
        ))]));
        let switched_primary = Arc::new(FakeModelProvider::new(vec![Ok(completed_text(
            "switched reply",
        ))]));
        let browser = Arc::new(FakeModelProvider::new(vec![Ok(completed_text(
            "browser reply",
        ))]));
        let mut catalog = SubagentRoleCatalog::builtin();
        catalog
            .apply_role_overlay(
                "browser",
                crate::SubagentRoleOverlay {
                    base: Some("review".to_owned()),
                    model_override: Some(crate::SubagentModelOverride {
                        provider: browser.clone(),
                        model: ModelName::new("browser-vision-model").expect("browser model valid"),
                        retry_policy: None,
                    }),
                    ..Default::default()
                },
            )
            .expect("browser role overlay valid");
        let policy = CodingRuntimePolicy::try_new(Vec::new(), CodingPermissionPolicy::default())
            .expect("policy valid");
        let mut factory = build_factory(temp.path(), primary.clone(), policy);
        factory.composition.role_catalog = catalog;

        let mut browser_input = child_input(
            "child-browser-only",
            vec![
                ToolName::new("browser_tab").expect("valid name"),
                ToolName::new("browser_network").expect("valid name"),
                ToolName::new("read_text").expect("valid name"),
            ],
        );
        browser_input.task = browser_input
            .task
            .with_role(Some("browser".to_owned()))
            .expect("valid role");
        let browser_child = factory
            .build_child(browser_input)
            .expect("browser child runtime builds");
        run_text_step(&browser_child, "Inspect preview page.").await;
        assert_eq!(browser.recorded_requests().len(), 1);
        assert_eq!(
            browser.recorded_requests()[0].model().as_str(),
            "browser-vision-model"
        );
        assert_eq!(primary.recorded_requests().len(), 0);

        // Live switched parent model takes effect immediately for roles without explicit model override.
        let mut switched_input = child_input(
            "child-switched",
            vec![ToolName::new("read_text").expect("valid name")],
        );
        switched_input.parent_primary_model = Some(merry_runtime::ParentPrimaryModel {
            provider: switched_primary.clone(),
            model: ModelName::new("live-switched-model").expect("valid"),
            retry_policy: ModelRetryPolicy::disabled(),
        });
        let switched_child = factory
            .build_child(switched_input)
            .expect("switched child runtime builds");
        run_text_step(&switched_child, "Use live parent model.").await;
        assert_eq!(switched_primary.recorded_requests().len(), 1);
        assert_eq!(
            switched_primary.recorded_requests()[0].model().as_str(),
            "live-switched-model"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writable_child_with_explicit_write_scope_keeps_accepted_process_session() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let run_call = ModelToolCall::new(
            ModelToolCallId::new("child-proc-call").expect("call id valid"),
            ToolName::new("run_process").expect("tool name valid"),
            ToolArguments::try_from(json!({
                "command": "mkdir -p out && printf subagent-ran > out/ok.txt"
            }))
            .expect("arguments valid"),
        );
        let primary = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(run_call)],
                FinishReason::ToolCalls,
                None,
            ),
        })]));
        let policy = CodingRuntimePolicy::try_new(Vec::new(), CodingPermissionPolicy::default())
            .expect("policy valid");
        let runner: Arc<dyn RuntimeProcessRunner> =
            Arc::new(TokioProcessRunner::new_at_workspace_root(temp.path()));
        let permissioned_factory = Arc::new(StaticPermissionedProcessRunnerFactory::new(
            Arc::clone(&runner),
        ));
        let session = ProcessSession::from_parts(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
            permissioned_factory,
        );
        let profile_builder = CodingAgentProfileBuilder::new(temp.path())
            .patch_tool()
            .retry_policy(ModelRetryPolicy::disabled())
            .accepted_process_session(session.clone());
        let factory = CodingChildRuntimeFactory::from_composition(CodingRuntimeComposition {
            profile_builder,
            provider: primary,
            model: ModelName::new("child-primary").expect("child model should be valid"),
            process_backend: Arc::new(LocalProcessBackend::from_session(session)),
            parent_process_permissions: None,
            parent_workspace_writable: true,
            subagent_config: SubagentConfig::new(1, 1)
                .expect("subagent config should be valid")
                .with_model_turn_bounds(1, 80)
                .expect("coding subagent bounds should be valid"),
            role_catalog: SubagentRoleCatalog::builtin(),
            automatic_compaction: CompactionConfig::disabled(),
            max_parallel_tool_calls: None,
            policy,
            worktree_manager: WorktreeIsolationManager::new(),
        });

        let task = SubagentTaskSpec::new("Run verification command.", 4)
            .expect("task valid")
            .with_write_scope(["src"])
            .expect("write scope valid");
        let scope = ChildWorkspaceScope::from_task(&task);
        let runtime = factory
            .build_child(ChildRuntimeInput {
                parent_session_id: SessionId::new("parent-writable-proc").expect("parent valid"),
                agent_id: None,
                session_id: SessionId::new("child-writable-proc").expect("session id valid"),
                session_store: None,
                loaded_session: None,
                existing_worktree: None,
                task_anchor: TaskAnchor::new("Run verification command.").expect("anchor valid"),
                task,
                allowed_tools: vec![ToolName::new("run_process").expect("tool name valid")],
                workspace_scope: scope,
                depth: 1,
                generation_config: GenerationConfig::default(),
                parent_primary_model: None,
                parent_generation_config: GenerationConfig::default(),
                plan_subagent_control: None,
                plan_subagent_scope: None,
                plan_link: None,
                plan_link_runtime: None,
                activity_hub: None,
            })
            .expect("writable child should build");

        run_text_step(&runtime, "Run command.").await;
        let pending = runtime.pending_tool_calls().await;
        assert_eq!(pending.len(), 1);
        let events = runtime
            .execute_tool_call(
                pending[0].id(),
                merry_runtime::ToolExecutionContext::default(),
            )
            .await
            .expect("child run_process should resolve");
        assert!(
            resolved_success(&events),
            "writable child with explicit write_scope must not be downgraded to read_only_process_runner: {events:?}"
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("out/ok.txt")).expect("child wrote file"),
            "subagent-ran"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_uses_dedicated_subagent_prompt_handoff_and_shared_role_cache_key() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn alpha() {}\npub fn beta() {}\npub fn gamma() {}\n",
        )
        .expect("seed file");

        let provider = Arc::new(FakeModelProvider::new(vec![Ok(completed_text("done"))]));
        let factory = build_factory(
            temp.path(),
            provider.clone(),
            CodingRuntimePolicy::try_new(Vec::new(), CodingPermissionPolicy::default())
                .expect("policy valid"),
        );

        let build_explore = |session: &str| {
            let task = SubagentTaskSpec::new("Find beta definition.", 8)
                .expect("valid task")
                .with_role(Some("explore".to_owned()))
                .expect("valid role")
                .with_read_scope(["."])
                .expect("read scope valid")
                .with_write_scope(Vec::<&str>::new())
                .expect("read-only scope")
                .with_context_files(["src/lib.rs:2-3"])
                .expect("valid context_files")
                .with_expected_output(Some("Return path:line only.".to_owned()));
            let scope = ChildWorkspaceScope::from_task(&task);
            factory
                .build_child(ChildRuntimeInput {
                    parent_session_id: SessionId::new("parent-cache-session")
                        .expect("parent session valid"),
                    agent_id: None,
                    session_id: SessionId::new(session).expect("child session valid"),
                    session_store: None,
                    loaded_session: None,
                    existing_worktree: None,
                    task_anchor: TaskAnchor::new("Find beta definition.").expect("anchor valid"),
                    task,
                    allowed_tools: vec![ToolName::new("read_text").expect("tool valid")],
                    workspace_scope: scope,
                    depth: 1,
                    generation_config: GenerationConfig::default(),
                    parent_primary_model: None,
                    parent_generation_config: GenerationConfig::default(),
                    plan_subagent_control: None,
                    plan_subagent_scope: None,
                    plan_link: None,
                    plan_link_runtime: None,
                    activity_hub: None,
                })
                .expect("explore child should build")
        };

        let child_one = build_explore("child-explore-1");
        let child_two = build_explore("child-explore-2");
        run_text_step(&child_one, "Find beta definition.").await;
        run_text_step(&child_two, "Find beta definition.").await;

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 2);

        let first_system_texts = requests[0]
            .input()
            .iter()
            .filter_map(|item| match item {
                merry_llm::ModelInputItem::Message(msg)
                    if msg.role() == merry_llm::ModelMessageRole::System =>
                {
                    Some(msg.content().as_text())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        assert!(first_system_texts.contains("<merry_subagent_instructions>"));
        assert!(!first_system_texts.contains("<merry_runtime_instructions>"));
        assert!(first_system_texts.contains("<merry_coding_subagent_policy>"));
        assert!(!first_system_texts.contains("Subagent orchestration discipline"));
        assert!(first_system_texts.contains("<merry_subagent_role>"));
        assert!(first_system_texts.contains("read-only codebase researcher"));
        assert!(
            !first_system_texts.contains("<merry_task_anchor>"),
            "child must not duplicate task in task_anchor"
        );
        assert!(first_system_texts.contains("<merry_subagent_handoff>"));
        assert!(
            first_system_texts
                .contains("<expected_output>\nReturn path:line only.\n</expected_output>")
        );
        assert!(first_system_texts.contains("<context_file path=\"src/lib.rs:2-3\">"));
        assert!(first_system_texts.contains("2: pub fn beta() {}"));

        // Stable prefixes of two sibling explore children must be byte-identical.
        let prefix_len = requests[0].stable_prefix_item_count();
        assert_eq!(prefix_len, requests[1].stable_prefix_item_count());
        assert_eq!(
            &requests[0].input()[..prefix_len],
            &requests[1].input()[..prefix_len],
            "sibling children of the same role must share a byte-identical stable prefix"
        );
    }
}
