use crate::{CodingAgentProfileBuilder, runtime::CodingRuntimePolicy};
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
    pub(crate) subagent_config: SubagentConfig,
    pub(crate) automatic_compaction: AutomaticCompactionConfig,
    pub(crate) policy: CodingRuntimePolicy,
}

impl CodingChildRuntimeFactory {
    pub(crate) fn from_composition(composition: CodingRuntimeComposition) -> Self {
        Self { composition }
    }
}

impl ChildRuntimeFactory for CodingChildRuntimeFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let process_session = self.composition.process_backend.new_session();
        let runner = process_session.runner();
        let allow_local_workspace_process = input
            .allowed_tools
            .iter()
            .any(|tool| tool.as_str() == CODING_LOOP_PROCESS_TOOL);
        let mut builder = Runtime::builder(input.session_id.clone())
            .task_anchor(input.task_anchor)
            .automatic_compaction(self.composition.automatic_compaction)
            .model_provider(
                Arc::clone(&self.composition.provider),
                self.composition.model.clone(),
            )
            .tool_admission(ToolAdmission::allow_only(input.allowed_tools.clone()));
        if let Some(activity_hub) = input.activity_hub.clone() {
            builder = builder.subagent_activity_hub(activity_hub);
        }
        let parent_plan_link_runtime = input.plan_link_runtime.clone();
        let child_factory: Arc<dyn ChildRuntimeFactory> = Arc::new(self.clone());
        let child_manager = SubagentManager::runtime_controlled_at_depth(
            input.session_id.clone(),
            self.composition.subagent_config,
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
            .composition
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
        let builder = profile.apply_to(builder)?;
        self.composition.policy.apply_to(builder).build()
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
        AcceptedLocalWorkspaceProcessAdmission, AutomaticCompactionConfig,
        CitationCompactionPolicy, PermissionAdmissionContext, PermissionAdmissionDecision,
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
            subagent_config: SubagentConfig::new(1, 1).expect("subagent config should be valid"),
            automatic_compaction: AutomaticCompactionConfig::disabled(),
            policy,
        })
    }

    fn child_input(session: &str, allowed_tools: Vec<ToolName>) -> ChildRuntimeInput {
        let task = SubagentTaskSpec::new("Exercise the child runtime.", 4)
            .expect("child task should be valid");
        ChildRuntimeInput {
            session_id: SessionId::new(session).expect("child session should be valid"),
            task_anchor: TaskAnchor::new("Exercise the child runtime.")
                .expect("child task anchor should be valid"),
            task,
            allowed_tools,
            workspace_scope: ChildWorkspaceScope::workspace_root(),
            depth: 0,
            generation_config: GenerationConfig::default(),
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
}
