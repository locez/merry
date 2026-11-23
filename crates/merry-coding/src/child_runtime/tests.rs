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
    PermissionAdmissionFuture, PermissionAdmissionSource, ProcessRunner as RuntimeProcessRunner,
    Runtime, RuntimeModelRole, StaticPermissionedProcessRunnerFactory, StepContext, StepInput,
    SubagentConfig, SubagentTaskSpec, TaskAnchor,
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn explicit_empty_write_scope_requests_read_only_child_workspace() {
    let catalog = SubagentRoleCatalog::builtin();
    let fallback_provider: Arc<dyn ModelProvider> = Arc::new(FakeModelProvider::new(Vec::new()));
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
    let permissioned_factory = Arc::new(StaticPermissionedProcessRunnerFactory::new(Arc::clone(
        &runner,
    )));
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
    let policy =
        CitationCompactionPolicy::new(None, None, 1).expect("compaction policy should be valid");
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
            ToolName::new("read_file").expect("valid name"),
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
        vec![ToolName::new("read_file").expect("valid name")],
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
    let permissioned_factory = Arc::new(StaticPermissionedProcessRunnerFactory::new(Arc::clone(
        &runner,
    )));
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
                allowed_tools: vec![ToolName::new("read_file").expect("tool valid")],
                workspace_scope: scope,
                depth: 1,
                generation_config: GenerationConfig::default(),
                parent_primary_model: None,
                parent_generation_config: GenerationConfig::default(),
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
