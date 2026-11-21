use crate::CODING_LOOP_PROCESS_TOOL;
use crate::{CodingAgentProfileBuilder, runtime::CodingRuntimePolicy};
use merry_core::ToolName;
use merry_llm::{ModelName, ModelProvider};
use merry_process::{ProcessBackend, ProcessSessionRequest};
use merry_runtime::{
    ChildRuntimeFactory, ChildRuntimeInput, ChildWorkspaceScope, CompactionConfig,
    LocalWorkspaceProcessSandboxProfile, ProcessSessionPermissionView, Runtime, RuntimeError,
    RuntimeModelRole, SubagentConfig, SubagentManager, ToolAdmission, subagent_registered_tools,
};
use merry_tools::{BROWSER_NETWORK_TOOL, BROWSER_TAB_TOOL};
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
    pub(crate) subagent_config: SubagentConfig,
    pub(crate) automatic_compaction: CompactionConfig,
    pub(crate) max_parallel_tool_calls: Option<NonZeroUsize>,
    pub(crate) policy: CodingRuntimePolicy,
}

impl CodingChildRuntimeFactory {
    pub(crate) fn from_composition(composition: CodingRuntimeComposition) -> Self {
        Self { composition }
    }
}

impl ChildRuntimeFactory for CodingChildRuntimeFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let write_scope_is_explicit = input.task.write_scope_is_explicit();
        let workspace_scope = input.workspace_scope;
        let child_workspace_writable =
            child_workspace_is_writable(&workspace_scope, write_scope_is_explicit);
        let mut session_request =
            ProcessSessionRequest::new().with_workspace_writable(child_workspace_writable);
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
        let allow_local_workspace_process = input
            .allowed_tools
            .iter()
            .any(|tool| tool.as_str() == CODING_LOOP_PROCESS_TOOL);
        let (child_provider, child_model) = if is_browser_subagent_toolset(&input.allowed_tools)
            && let Some(browser_role) = self.composition.policy.role(RuntimeModelRole::Browser)
        {
            (browser_role.provider(), browser_role.model().clone())
        } else {
            (
                Arc::clone(&self.composition.provider),
                self.composition.model.clone(),
            )
        };
        let mut builder = Runtime::builder(input.session_id.clone())
            .task_anchor(input.task_anchor)
            .automatic_compaction(self.composition.automatic_compaction.clone())
            .model_provider(child_provider, child_model)
            .tool_admission(ToolAdmission::allow_only(input.allowed_tools.clone()));
        if let Some(limit) = self.composition.max_parallel_tool_calls {
            builder = builder.max_parallel_tool_calls(limit);
        }
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
        let memory = self.composition.profile_builder.durable_memory_handle();
        let mut profile = self
            .composition
            .profile_builder
            .clone()
            .patch_write_scope(workspace_scope.write_scope().to_vec())
            .forbidden_paths(workspace_scope.forbidden_paths().to_vec())
            .register_tools([spawn_tool, wait_tool, cancel_tool]);
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
        self.composition.policy.apply_to(builder).build()
    }
}

fn child_workspace_is_writable(
    workspace_scope: &ChildWorkspaceScope,
    write_scope_is_explicit: bool,
) -> bool {
    !(write_scope_is_explicit && workspace_scope.write_scope().is_empty())
}

fn is_browser_subagent_toolset(allowed_tools: &[ToolName]) -> bool {
    let uses_browser = allowed_tools
        .iter()
        .any(|tool| matches!(tool.as_str(), BROWSER_TAB_TOOL | BROWSER_NETWORK_TOOL));
    let allows_code_mutation_or_process = allowed_tools
        .iter()
        .any(|tool| matches!(tool.as_str(), CODING_LOOP_PROCESS_TOOL | "apply_patch"));
    uses_browser && !allows_code_mutation_or_process
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
        AcceptedLocalWorkspaceProcessAdmission, CitationCompactionPolicy, CompactionConfig,
        PermissionAdmissionContext, PermissionAdmissionDecision, PermissionAdmissionFuture,
        PermissionAdmissionSource, ProcessRunner as RuntimeProcessRunner, Runtime,
        RuntimeModelRole, StaticPermissionedProcessRunnerFactory, StepContext, StepInput,
        SubagentConfig, SubagentTaskSpec, TaskAnchor,
    };
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn explicit_empty_write_scope_requests_read_only_child_workspace() {
        let task = SubagentTaskSpec::new("Read the assigned files.", 1)
            .expect("valid task")
            .with_write_scope(Vec::<&str>::new())
            .expect("empty write scope is valid");
        let scope = ChildWorkspaceScope::from_task(&task);

        assert!(!child_workspace_is_writable(
            &scope,
            task.write_scope_is_explicit()
        ));

        let writable_task = SubagentTaskSpec::new("Edit assigned files.", 1)
            .expect("valid task")
            .with_write_scope(["src"])
            .expect("write scope is valid");
        let writable_scope = ChildWorkspaceScope::from_task(&writable_task);
        assert!(child_workspace_is_writable(
            &writable_scope,
            writable_task.write_scope_is_explicit()
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
            parent_process_permissions: None,
            subagent_config: SubagentConfig::new(1, 1)
                .expect("subagent config should be valid")
                .with_model_turn_bounds(2048, 2048)
                .expect("coding subagent bounds should be valid"),
            automatic_compaction: CompactionConfig::disabled(),
            max_parallel_tool_calls: None,
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

    #[tokio::test(flavor = "current_thread")]
    async fn child_runtime_routes_browser_only_subagents_to_browser_role_when_configured() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let primary = Arc::new(FakeModelProvider::new(vec![Ok(completed_text(
            "primary reply",
        ))]));
        let browser = Arc::new(FakeModelProvider::new(vec![Ok(completed_text(
            "browser reply",
        ))]));
        let browser_role = CodingModelRoleConfig::new(
            RuntimeModelRole::Browser,
            browser.clone(),
            ModelName::new("browser-vision-model").expect("browser model valid"),
        )
        .expect("browser role valid");
        let policy =
            CodingRuntimePolicy::try_new(vec![browser_role], CodingPermissionPolicy::default())
                .expect("policy valid");
        let factory = build_factory(temp.path(), primary.clone(), policy);

        let browser_child = factory
            .build_child(child_input(
                "child-browser-only",
                vec![
                    ToolName::new("browser_tab").expect("valid name"),
                    ToolName::new("browser_network").expect("valid name"),
                    ToolName::new("read_text").expect("valid name"),
                ],
            ))
            .expect("browser child runtime builds");
        run_text_step(&browser_child, "Inspect preview page.").await;
        assert_eq!(browser.recorded_requests().len(), 1);
        assert_eq!(
            browser.recorded_requests()[0].model().as_str(),
            "browser-vision-model"
        );
        assert_eq!(primary.recorded_requests().len(), 0);

        let mixed_child = factory
            .build_child(child_input(
                "child-mixed",
                vec![
                    ToolName::new("browser_tab").expect("valid name"),
                    ToolName::new("run_process").expect("valid name"),
                ],
            ))
            .expect("mixed child runtime builds");
        run_text_step(&mixed_child, "Fix bug and check UI.").await;
        assert_eq!(primary.recorded_requests().len(), 1);
        assert_eq!(
            primary.recorded_requests()[0].model().as_str(),
            "child-primary"
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
            subagent_config: SubagentConfig::new(1, 1)
                .expect("subagent config should be valid")
                .with_model_turn_bounds(2048, 2048)
                .expect("coding subagent bounds should be valid"),
            automatic_compaction: CompactionConfig::disabled(),
            max_parallel_tool_calls: None,
            policy,
        });

        let task = SubagentTaskSpec::new("Run verification command.", 4)
            .expect("task valid")
            .with_write_scope(["src"])
            .expect("write scope valid");
        let scope = ChildWorkspaceScope::from_task(&task);
        let runtime = factory
            .build_child(ChildRuntimeInput {
                session_id: SessionId::new("child-writable-proc").expect("session id valid"),
                task_anchor: TaskAnchor::new("Run verification command.").expect("anchor valid"),
                task,
                allowed_tools: vec![ToolName::new("run_process").expect("tool name valid")],
                workspace_scope: scope,
                depth: 1,
                generation_config: GenerationConfig::default(),
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
}
