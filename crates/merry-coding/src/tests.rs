use super::*;
use crate::runtime::CodingRuntimePolicy;
use futures_util::stream;
use merry_core::{
    ProviderName, SessionId, SubagentActivityPhase, ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse, ModelRetryPolicy,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ProviderErrorKind, ToolArguments,
    testing::FakeModelProvider,
};
use merry_process::{LocalProcessBackend, ProcessSession, TokioProcessRunner};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, AgentLoopConfig, AgentLoopStatus,
    PermissionAdmissionContext, PermissionAdmissionDecision, PermissionAdmissionFuture,
    PermissionAdmissionSource, PermissionedProcessRunnerFactory, ProcessRunner, RegisteredTool,
    RuntimeModelRole, StaticPermissionedProcessRunnerFactory, StepContext, StepInput,
    SubagentConfig,
};
use schemars::Schema;
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn bridge_tool(name: &str) -> RegisteredTool {
    bridge_tool_with_description(name, "Test bridge tool")
}

fn bridge_tool_with_description(name: &str, description: &str) -> RegisteredTool {
    let schema =
        Schema::try_from(json!({ "type": "object" })).expect("test schema should be valid");
    let spec = ToolSpec::new(
        ToolName::new(name).expect("test tool name should be valid"),
        description,
        ToolInputSchema::new(schema).expect("test input schema should be valid"),
    )
    .expect("test tool spec should be valid");
    RegisteredTool::bridge(spec)
}

fn process_session(
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
) -> ProcessSession {
    let permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory> = Arc::new(
        StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
    );
    ProcessSession::from_parts(admission, runner, permissioned_factory)
}

#[test]
fn workspace_profile_builds_read_tools_and_coding_defaults() {
    let temp = tempfile::tempdir().expect("tempdir should be created");

    let profile = coding_agent(temp.path())
        .build()
        .expect("workspace profile should build");

    let tool_names = profile
        .registered_tools()
        .iter()
        .map(|tool| tool.spec().name().as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"workspace_read_file"));
    assert!(tool_names.contains(&"workspace_list_dir"));
    assert!(tool_names.contains(&"workspace_search_text"));
    assert!(!tool_names.contains(&"workspace_patch"));
    assert!(profile.runtime_profile().progress_commentary());
    assert_eq!(
        profile.runtime_profile().model_retry_policy(),
        Some(ModelRetryPolicy::coding_agent_default())
    );
}

#[test]
fn coding_agent_profile_has_one_canonical_workspace_tool_order() {
    let temp = tempfile::tempdir().expect("tempdir should be created");

    let profile = coding_agent(temp.path())
        .build()
        .expect("coding-agent profile should build");
    let tool_names = profile
        .tool_names()
        .into_iter()
        .map(ToolName::as_str)
        .collect::<Vec<_>>();

    assert_eq!(
        tool_names,
        vec![
            "workspace_read_file",
            "workspace_list_dir",
            "workspace_search_text",
        ]
    );
}

#[test]
fn coding_agent_profile_owns_process_permission_and_patch_order() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let runner: Arc<dyn merry_runtime::ProcessRunner> = Arc::new(TokioProcessRunner::new());
    let profile = coding_agent(temp.path())
        .patch_tool()
        .accepted_process_session(process_session(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner,
        ))
        .build()
        .expect("coding-agent profile should build");

    let tool_names = profile
        .tool_names()
        .into_iter()
        .map(ToolName::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        tool_names,
        vec![
            "run_process",
            "request_permissions",
            "workspace_read_file",
            "workspace_list_dir",
            "workspace_search_text",
            "workspace_patch",
        ]
    );
}

#[test]
fn coding_agent_profile_hash_is_deterministic_and_tracks_stable_material() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let first = coding_agent(temp.path())
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use the project rules.")
                .expect("project rules should be valid"),
        )
        .task_anchor(TaskAnchor::new("First task").expect("task anchor should be valid"))
        .build()
        .expect("coding-agent profile should build");
    let dynamic_change = coding_agent(temp.path())
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use the project rules.")
                .expect("project rules should be valid"),
        )
        .task_anchor(TaskAnchor::new("Different task").expect("task anchor should be valid"))
        .build()
        .expect("coding-agent profile should build");
    let stable_change = coding_agent(temp.path())
        .project_rules(
            ProjectRules::new("AGENTS.md", "Changed project rules.")
                .expect("project rules should be valid"),
        )
        .task_anchor(TaskAnchor::new("First task").expect("task anchor should be valid"))
        .build()
        .expect("coding-agent profile should build");
    let retry_change = coding_agent(temp.path())
        .retry_policy(ModelRetryPolicy::disabled())
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use the project rules.")
                .expect("project rules should be valid"),
        )
        .task_anchor(TaskAnchor::new("First task").expect("task anchor should be valid"))
        .build()
        .expect("coding-agent profile should build");

    assert_eq!(first.profile_hash(), dynamic_change.profile_hash());
    assert_ne!(first.profile_hash(), stable_change.profile_hash());
    assert_ne!(first.profile_hash(), retry_change.profile_hash());
    assert!(first.profile_hash().as_str().starts_with("fnv1a64:"));
}

#[test]
fn coding_agent_profile_hash_includes_advertised_tool_order() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let first = coding_agent(temp.path())
        .register_tools([bridge_tool("alpha"), bridge_tool("beta")])
        .build()
        .expect("coding-agent profile should build");
    let reordered = coding_agent(temp.path())
        .register_tools([bridge_tool("beta"), bridge_tool("alpha")])
        .build()
        .expect("coding-agent profile should build");

    assert_ne!(first.profile_hash(), reordered.profile_hash());
    assert_eq!(
        first
            .tool_names()
            .into_iter()
            .map(ToolName::as_str)
            .collect::<Vec<_>>(),
        vec![
            "workspace_read_file",
            "workspace_list_dir",
            "workspace_search_text",
            "alpha",
            "beta"
        ]
    );
}

#[test]
fn coding_agent_profile_hash_tracks_tool_schema_and_coding_run_policy() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let base = coding_agent(temp.path())
        .register_tool(bridge_tool("catalog_tool"))
        .build()
        .expect("base profile should build");
    let changed_schema = coding_agent(temp.path())
        .register_tool(bridge_tool_with_description(
            "catalog_tool",
            "Changed description",
        ))
        .build()
        .expect("changed schema profile should build");
    let changed_policy = coding_agent(temp.path())
        .run_policy(
            CodingAgentRunPolicy::new(16, CodingFinalReportPolicy::EvidenceBackedSummary)
                .expect("valid coding run policy"),
        )
        .build()
        .expect("changed policy profile should build");

    assert_ne!(base.profile_hash(), changed_schema.profile_hash());
    assert_ne!(base.profile_hash(), changed_policy.profile_hash());
    assert_eq!(base.run_policy().max_model_turns(), 1024);
    assert_eq!(
        changed_policy
            .loop_config()
            .expect("loop config")
            .max_model_turns(),
        16
    );
    assert_eq!(
        changed_policy.run_policy().final_report().as_str(),
        "evidence_backed_summary"
    );
}

#[test]
fn coding_agent_profile_owns_the_coding_prompt_and_hashes_its_exact_text() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let profile = coding_agent(temp.path())
        .build()
        .expect("coding-agent profile should build");
    let runtime_profile = profile.runtime_profile();
    let prompt = runtime_profile.prompt_profile();

    assert_eq!(prompt.stable_blocks().len(), 1);
    assert_eq!(prompt.stable_blocks()[0].tag(), "merry_coding_policy");
    assert_eq!(prompt.stable_blocks()[0].text(), CODING_AGENT_POLICY_PROMPT);
    assert!(
        prompt.stable_blocks()[0]
            .text()
            .contains("evidence-backed summary")
    );
}

#[test]
fn coding_agent_profile_hash_distinguishes_patch_scope_and_process_admission() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let runner: Arc<dyn merry_runtime::ProcessRunner> = Arc::new(TokioProcessRunner::new());
    let unrestricted = coding_agent(temp.path())
        .patch_tool()
        .build()
        .expect("unrestricted profile should build");
    let read_only = coding_agent(temp.path())
        .patch_tool()
        .read_only_patch_scope()
        .build()
        .expect("read-only profile should build");
    let isolated = coding_agent(temp.path())
        .accepted_process_session(process_session(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            Arc::clone(&runner),
        ))
        .build()
        .expect("isolated profile should build");
    let host = coding_agent(temp.path())
        .accepted_process_session(process_session(
            AcceptedLocalWorkspaceProcessAdmission::accept_host(),
            runner,
        ))
        .build()
        .expect("host profile should build");

    assert_ne!(unrestricted.profile_hash(), read_only.profile_hash());
    assert_ne!(isolated.profile_hash(), host.profile_hash());
}

#[test]
fn workspace_profile_can_enable_patch_tool() {
    let temp = tempfile::tempdir().expect("tempdir should be created");

    let profile = coding_agent(temp.path())
        .patch_tool()
        .read_only_patch_scope()
        .build()
        .expect("workspace profile should build");

    assert!(
        profile
            .registered_tools()
            .iter()
            .any(|tool| tool.spec().name().as_str() == "workspace_patch")
    );
}

fn completing_provider() -> Arc<FakeModelProvider> {
    Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]))
}

fn process_backend() -> Arc<dyn merry_process::ProcessBackend> {
    let runner: Arc<dyn ProcessRunner> = Arc::new(TokioProcessRunner::new());
    let session = process_session(
        AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
        runner,
    );
    Arc::new(LocalProcessBackend::from_session(session))
}

#[derive(Clone)]
struct ParentChildProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    state: Arc<Mutex<ParentChildProviderState>>,
}

struct ParentChildProviderState {
    parent_turns: usize,
    child_turns: usize,
    requests: Vec<ModelRequest>,
}

impl ParentChildProvider {
    fn new() -> Self {
        Self {
            name: ProviderName::new("parent-child-provider")
                .expect("provider name should be valid"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("provider capabilities should be valid"),
            state: Arc::new(Mutex::new(ParentChildProviderState {
                parent_turns: 0,
                child_turns: 0,
                requests: Vec::new(),
            })),
        }
    }

    fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.state
            .lock()
            .expect("provider state should not be poisoned")
            .requests
            .clone()
    }
}

impl ModelProvider for ParentChildProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let is_child = request.messages().iter().any(|message| {
                message
                    .content()
                    .as_text()
                    .contains("Run the child permission task.")
            });
            let event = {
                let mut state = self
                    .state
                    .lock()
                    .expect("provider state should not be poisoned");
                state.requests.push(request);
                if is_child {
                    let event = if state.child_turns == 0 {
                        permission_call_event()
                    } else {
                        completed_text("child done")
                    };
                    state.child_turns += 1;
                    event
                } else {
                    let event = if state.parent_turns == 0 {
                        spawn_child_event()
                    } else {
                        completed_text("parent done")
                    };
                    state.parent_turns += 1;
                    event
                }
            };
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as ModelEventStream)
        })
    }
}

fn completed_text(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

fn spawn_child_event() -> ModelEvent {
    let call = ModelToolCall::new(
        ModelToolCallId::new("parent-spawn-child").expect("call id should be valid"),
        ToolName::new("spawn_subagents").expect("tool name should be valid"),
        ToolArguments::try_from(json!({
            "tasks": [{
                "task": "Run the child permission task.",
                "max_model_turns": 2,
                "allowed_tools": ["request_permissions"],
                "write_scope": []
            }]
        }))
        .expect("spawn arguments should be valid"),
    );
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    }
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

#[derive(Clone)]
struct CountingAdmission {
    calls: Arc<AtomicUsize>,
}

impl CountingAdmission {
    fn approving() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
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
            Ok(PermissionAdmissionDecision::approved("host approved"))
        })
    }
}

async fn run_parent_child_policy(
    session_id: &str,
    permission: CodingPermissionPolicy,
    model_roles: Vec<CodingModelRoleConfig>,
) -> Arc<ParentChildProvider> {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let primary = Arc::new(ParentChildProvider::new());
    let input = CodingRuntimeInput::new(
        SessionId::new(session_id).expect("session id should be valid"),
        temp.path(),
        primary.clone(),
        ModelName::new("parent-primary").expect("primary model should be valid"),
        process_backend(),
    )
    .with_automatic_compaction(merry_runtime::AutomaticCompactionConfig::disabled())
    .with_retry_policy(ModelRetryPolicy::disabled())
    .with_model_roles(model_roles)
    .with_subagents(CodingSubagentsConfig::enabled(
        SubagentConfig::new(1, 1).expect("subagent limits should be valid"),
    ));
    let coding_runtime = CodingRuntimeBuilder::new(input)
        .permission_policy(permission)
        .build()
        .expect("parent coding runtime should build");
    let mut activity = coding_runtime.runtime().subscribe_subagent_activity();

    let result = coding_runtime
        .runtime()
        .run_agent_loop(
            StepInput::user_text("Delegate the child permission task.")
                .expect("input should be valid"),
            StepContext::default(),
            AgentLoopConfig::new(3).expect("loop config should be valid"),
        )
        .await
        .expect("parent loop should complete");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    let snapshots = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshots = activity.borrow_and_update().clone();
            if snapshots
                .iter()
                .any(|snapshot| matches!(snapshot.phase, SubagentActivityPhase::Completed))
            {
                break snapshots;
            }
            assert!(
                !snapshots.iter().any(|snapshot| {
                    matches!(
                        snapshot.phase,
                        SubagentActivityPhase::Failed | SubagentActivityPhase::Cancelled
                    )
                }),
                "child runtime should not terminate unsuccessfully: {snapshots:?}"
            );
            activity
                .changed()
                .await
                .expect("subagent activity stream should remain open");
        }
    })
    .await
    .expect("child runtime should complete before timeout");
    assert!(
        snapshots
            .iter()
            .any(|snapshot| matches!(snapshot.phase, SubagentActivityPhase::Completed))
    );
    primary
}

#[tokio::test(flavor = "current_thread")]
async fn parent_builder_composes_full_coding_runtime_and_loop_policy() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let provider = completing_provider();
    let provider_input: Arc<dyn ModelProvider> = provider.clone();
    let model = ModelName::new("debug-model").expect("model name should be valid");
    let subagents = CodingSubagentsConfig::enabled(
        SubagentConfig::new(2, 1).expect("subagent limits should be valid"),
    );
    let input = CodingRuntimeInput::new(
        SessionId::new("coding-parent-builder").expect("session id should be valid"),
        temp.path(),
        provider_input.clone(),
        model.clone(),
        process_backend(),
    )
    .with_automatic_compaction(merry_runtime::AutomaticCompactionConfig::disabled())
    .with_retry_policy(ModelRetryPolicy::disabled())
    .with_model_role(
        CodingModelRoleConfig::new(RuntimeModelRole::ContextCompaction, provider_input, model)
            .expect("secondary model role should be valid"),
    )
    .with_subagents(subagents);

    let coding_runtime = CodingRuntimeBuilder::new(input)
        .build()
        .expect("full coding runtime should build");
    assert_eq!(
        coding_runtime.loop_config().max_model_turns(),
        DEFAULT_CODING_AGENT_MAX_MODEL_TURNS
    );
    let tool_names = coding_runtime
        .profile()
        .tool_names()
        .into_iter()
        .map(ToolName::as_str)
        .collect::<Vec<_>>();
    for tool in [
        "run_process",
        "request_permissions",
        "workspace_patch",
        "spawn_subagents",
        "wait_subagents",
        "cancel_subagents",
    ] {
        assert!(tool_names.contains(&tool), "missing composed tool {tool}");
    }

    let loop_config = coding_runtime.loop_config();
    let result = coding_runtime
        .runtime()
        .run_agent_loop(
            StepInput::user_text("Inspect the workspace.").expect("input should be valid"),
            StepContext::default(),
            loop_config,
        )
        .await
        .expect("coding loop should complete");
    assert!(matches!(
        result.status(),
        merry_runtime::AgentLoopStatus::Completed
    ));
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn parent_builder_passes_policy_to_a_real_child_runtime() {
    let approval = Arc::new(FakeModelProvider::new(vec![Ok(approval_event())]));
    let approval_role = CodingModelRoleConfig::new(
        RuntimeModelRole::ApprovalReview,
        approval.clone(),
        ModelName::new("child-approval").expect("approval model should be valid"),
    )
    .expect("approval role should be valid");
    let primary = run_parent_child_policy(
        "coding-parent-child-policy",
        CodingPermissionPolicy::model_only(),
        vec![approval_role],
    )
    .await;
    assert_eq!(approval.recorded_requests().len(), 1);
    assert_eq!(
        approval.recorded_requests()[0].model().as_str(),
        "child-approval"
    );
    assert!(primary.recorded_requests().len() >= 3);
}

#[test]
fn process_boundary_policy_rejects_missing_host_admission() {
    let error = match CodingPermissionPolicy::for_process_boundary(
        CodingProcessBoundary::Unrestricted,
        CodingTrustMode::Reviewed,
        NoSandboxReviewMode::Model,
        None,
    ) {
        Ok(_) => panic!("headless host fallback must be explicit"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        CodingPermissionPolicyError::HostAdmissionUnavailable {
            boundary: CodingProcessBoundary::Unrestricted
        }
    ));
}

#[test]
fn process_boundary_policy_uses_model_then_host_only_for_interactive_no_sandbox() {
    let host = CountingAdmission::approving();
    let policy = CodingPermissionPolicy::for_process_boundary(
        CodingProcessBoundary::Unrestricted,
        CodingTrustMode::Reviewed,
        NoSandboxReviewMode::Model,
        Some(Arc::new(host)),
    )
    .expect("host fallback policy should build");

    assert!(matches!(
        policy,
        CodingPermissionPolicy::ModelThenHostFallback { .. }
    ));
}

#[test]
fn process_boundary_policy_defaults_to_host_review_for_interactive_no_sandbox() {
    let host = CountingAdmission::approving();
    let policy = CodingPermissionPolicy::for_process_boundary(
        CodingProcessBoundary::Unrestricted,
        CodingTrustMode::Reviewed,
        NoSandboxReviewMode::Host,
        Some(Arc::new(host)),
    )
    .expect("host review policy should build");

    assert!(matches!(
        policy,
        CodingPermissionPolicy::HostDecisionOnly { .. }
    ));
}

#[test]
fn process_boundary_policy_keeps_outer_and_inner_model_only() {
    let host = CountingAdmission::approving();
    let policy = CodingPermissionPolicy::for_process_boundary(
        CodingProcessBoundary::OuterAndInner,
        CodingTrustMode::Reviewed,
        NoSandboxReviewMode::Model,
        Some(Arc::new(host)),
    )
    .expect("outer and inner model-only policy should build");

    assert!(matches!(policy, CodingPermissionPolicy::Required));
}

#[test]
fn outer_and_inner_model_only_does_not_need_a_host_admission_source() {
    let policy = CodingPermissionPolicy::for_process_boundary(
        CodingProcessBoundary::OuterAndInner,
        CodingTrustMode::Reviewed,
        NoSandboxReviewMode::Model,
        None,
    )
    .expect("outer and inner should remain model-only without host fallback");

    assert!(matches!(policy, CodingPermissionPolicy::Required));
}

#[test]
fn fully_trusted_process_policy_does_not_need_a_host_admission_source() {
    let policy = CodingPermissionPolicy::for_process_boundary(
        CodingProcessBoundary::Unrestricted,
        CodingTrustMode::FullyTrusted,
        NoSandboxReviewMode::Host,
        None,
    )
    .expect("fully trusted mode should not need a reviewer");

    assert!(matches!(policy, CodingPermissionPolicy::FullyTrusted));
}

#[tokio::test(flavor = "current_thread")]
async fn parent_builder_passes_host_admission_to_a_real_child_runtime() {
    let host = CountingAdmission::approving();
    let primary = run_parent_child_policy(
        "coding-parent-child-host-policy",
        CodingPermissionPolicy::host_decision_only(Arc::new(host.clone())),
        Vec::new(),
    )
    .await;
    assert_eq!(host.calls(), 1);
    assert!(primary.recorded_requests().len() >= 3);
}

#[tokio::test(flavor = "current_thread")]
async fn parent_builder_passes_model_fallback_to_a_real_child_runtime() {
    let approval = Arc::new(FakeModelProvider::new(vec![Err(ModelError::provider(
        ProviderErrorKind::Unavailable,
        "approval provider unavailable",
    ))]));
    let approval_role = CodingModelRoleConfig::new(
        RuntimeModelRole::ApprovalReview,
        approval.clone(),
        ModelName::new("child-approval-fallback").expect("approval fallback model should be valid"),
    )
    .expect("approval role should be valid");
    let host = CountingAdmission::approving();
    let primary = run_parent_child_policy(
        "coding-parent-child-fallback-policy",
        CodingPermissionPolicy::model_then_host_fallback(Arc::new(host.clone())),
        vec![approval_role],
    )
    .await;

    assert_eq!(approval.recorded_requests().len(), 1);
    assert_eq!(host.calls(), 1);
    assert!(primary.recorded_requests().len() >= 3);
}

#[tokio::test(flavor = "current_thread")]
async fn parent_builder_passes_fully_trusted_to_a_real_child_runtime() {
    let approval = Arc::new(FakeModelProvider::new(vec![Err(ModelError::provider(
        ProviderErrorKind::Unavailable,
        "approval provider must not be called",
    ))]));
    let approval_role = CodingModelRoleConfig::new(
        RuntimeModelRole::ApprovalReview,
        approval.clone(),
        ModelName::new("child-approval-trusted").expect("trusted approval model should be valid"),
    )
    .expect("approval role should be valid");
    let primary = run_parent_child_policy(
        "coding-parent-child-trusted-policy",
        CodingPermissionPolicy::fully_trusted(),
        vec![approval_role],
    )
    .await;

    assert!(approval.recorded_requests().is_empty());
    assert!(primary.recorded_requests().len() >= 3);
}

#[tokio::test(flavor = "current_thread")]
async fn command_generation_builder_is_read_only_even_with_full_policy_inputs() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let provider = completing_provider();
    let model = ModelName::new("debug-model").expect("model name should be valid");
    let input = CodingRuntimeInput::new(
        SessionId::new("coding-command-builder").expect("session id should be valid"),
        temp.path(),
        provider.clone(),
        model,
        process_backend(),
    )
    .with_subagents(CodingSubagentsConfig::enabled(
        SubagentConfig::new(2, 1).expect("subagent limits should be valid"),
    ));

    let coding_runtime = CodingRuntimeBuilder::for_command_generation(input)
        .build()
        .expect("command generation runtime should build");
    let tool_names = coding_runtime
        .profile()
        .tool_names()
        .into_iter()
        .map(ToolName::as_str)
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"workspace_read_file"));
    for tool in [
        "run_process",
        "request_permissions",
        "workspace_patch",
        "spawn_subagents",
        "wait_subagents",
        "cancel_subagents",
    ] {
        assert!(
            !tool_names.contains(&tool),
            "read-only runtime exposed {tool}"
        );
    }

    let result = coding_runtime
        .runtime()
        .run_agent_loop(
            StepInput::user_text("Describe the workspace.").expect("input should be valid"),
            StepContext::default(),
            coding_runtime.loop_config(),
        )
        .await
        .expect("read-only loop should complete");
    assert!(matches!(
        result.status(),
        merry_runtime::AgentLoopStatus::Completed
    ));
}

#[test]
fn full_builder_rejects_missing_process_backend() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let provider = completing_provider();
    let input = CodingRuntimeInput::read_only(
        SessionId::new("coding-missing-process").expect("session id should be valid"),
        temp.path(),
        provider,
        ModelName::new("debug-model").expect("model name should be valid"),
    );

    let error = match CodingRuntimeBuilder::new(input).build() {
        Ok(_) => panic!("full coding runtime must require a process backend"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CodingRuntimeBuildError::MissingProcessBackend
    ));
}

#[test]
fn coding_runtime_policy_rejects_duplicate_model_roles_at_its_boundary() {
    let provider: Arc<dyn ModelProvider> = completing_provider();
    let model = ModelName::new("debug-model").expect("model name should be valid");
    let role = CodingModelRoleConfig::new(
        RuntimeModelRole::ContextCompaction,
        Arc::clone(&provider),
        model,
    )
    .expect("secondary model role should be valid");

    let error = match CodingRuntimePolicy::try_new(
        vec![role.clone(), role],
        CodingPermissionPolicy::default(),
    ) {
        Ok(_) => panic!("duplicate model roles must be rejected by the policy owner"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CodingRuntimeBuildError::DuplicateModelRole {
            role: RuntimeModelRole::ContextCompaction
        }
    ));
}

#[test]
fn parent_builder_rejects_ambiguous_model_roles() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let provider = completing_provider();
    let provider_input: Arc<dyn ModelProvider> = provider.clone();
    let model = ModelName::new("debug-model").expect("model name should be valid");
    let role = CodingModelRoleConfig::new(
        RuntimeModelRole::ContextCompaction,
        provider_input.clone(),
        model.clone(),
    )
    .expect("secondary model role should be valid");
    let duplicate_input = CodingRuntimeInput::read_only(
        SessionId::new("coding-duplicate-role").expect("session id should be valid"),
        temp.path(),
        provider_input.clone(),
        model.clone(),
    )
    .with_model_roles([role.clone(), role]);
    let duplicate_error =
        match CodingRuntimeBuilder::for_command_generation(duplicate_input).build() {
            Ok(_) => panic!("duplicate model roles must be rejected"),
            Err(error) => error,
        };
    assert!(matches!(
        duplicate_error,
        CodingRuntimeBuildError::DuplicateModelRole {
            role: RuntimeModelRole::ContextCompaction
        }
    ));

    let primary_error = match CodingModelRoleConfig::new(RuntimeModelRole::Primary, provider, model)
    {
        Ok(_) => panic!("primary role must be rejected at construction"),
        Err(error) => error,
    };
    assert!(matches!(
        primary_error,
        CodingModelRoleConfigError::PrimaryRole
    ));
}
