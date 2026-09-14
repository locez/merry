use crate::{
    CodingApprovalPolicy, CodingModelRoleConfig, CodingPermissionPolicy,
    CodingPermissionPolicyError, CodingRuntimeBuilder, CodingRuntimeInput, CodingSubagentsConfig,
    tests::process_backend,
};
use futures_util::stream;
use merry_core::{ProviderName, SessionId, SubagentActivityPhase, ToolName};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse, ModelRetryPolicy,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ProviderErrorKind, ToolArguments,
    testing::FakeModelProvider,
};
use merry_runtime::{
    AgentLoopConfig, AgentLoopStatus, PermissionAdmissionContext, PermissionAdmissionDecision,
    PermissionAdmissionFuture, PermissionAdmissionSource, RuntimeModelRole, StepContext, StepInput,
    SubagentConfig,
};
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

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
                "max_model_turns": 2048,
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
        SubagentConfig::new(1, 1)
            .expect("subagent limits should be valid")
            .with_model_turn_bounds(2048, 2048)
            .expect("coding subagent bounds should be valid"),
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
fn host_reviewed_policies_reject_missing_host_admission() {
    for policy in [
        CodingApprovalPolicy::ModelThenHuman,
        CodingApprovalPolicy::HumanOnly,
    ] {
        let error = match CodingPermissionPolicy::for_approval_policy(policy, None) {
            Ok(_) => panic!("{policy:?} needs a host source"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                CodingPermissionPolicyError::HostAdmissionUnavailable { policy: found }
                    if found == policy
            ),
            "{policy:?}"
        );
    }
}

#[test]
fn model_then_human_policy_uses_host_fallback() {
    let policy = CodingPermissionPolicy::for_approval_policy(
        CodingApprovalPolicy::ModelThenHuman,
        Some(Arc::new(CountingAdmission::approving())),
    )
    .expect("host fallback policy should build");

    assert!(matches!(
        policy,
        CodingPermissionPolicy::ModelThenHostFallback { .. }
    ));
}

#[test]
fn human_only_policy_uses_host_decision() {
    let policy = CodingPermissionPolicy::for_approval_policy(
        CodingApprovalPolicy::HumanOnly,
        Some(Arc::new(CountingAdmission::approving())),
    )
    .expect("host review policy should build");

    assert!(matches!(
        policy,
        CodingPermissionPolicy::HostDecisionOnly { .. }
    ));
}

#[test]
fn reviewer_free_policies_do_not_need_a_host_admission_source() {
    let model = CodingPermissionPolicy::for_approval_policy(CodingApprovalPolicy::ModelOnly, None)
        .expect("model-only should not need a reviewer");
    assert!(matches!(model, CodingPermissionPolicy::Required));

    let none = CodingPermissionPolicy::for_approval_policy(CodingApprovalPolicy::NoApproval, None)
        .expect("no-approval should not need a reviewer");
    assert!(matches!(none, CodingPermissionPolicy::FullyTrusted));

    let deny = CodingPermissionPolicy::for_approval_policy(CodingApprovalPolicy::Deny, None)
        .expect("deny should not need a reviewer");
    assert!(matches!(deny, CodingPermissionPolicy::DenyAll));
}

#[test]
fn default_approval_policy_is_model_then_human() {
    assert_eq!(
        CodingApprovalPolicy::default(),
        CodingApprovalPolicy::ModelThenHuman
    );
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
