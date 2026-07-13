use super::*;
use crate::plan::{
    BeginPlanInput, ControlPlanAttemptInput, PlanChangeInput, PlanDecompositionInput,
    PlanExecutionIntent, PlanNodeInput, ReportPlanAttemptInput, ReportPlanProgressInput,
    UpdatePlanInput,
};
use crate::{ChildRuntimeFactory, ChildRuntimeInput};
use merry_core::{
    PlanAttemptOutcome, PlanCapabilityEnvelopeSnapshot, PlanDirectiveConstraints,
    PlanDirectiveKind, PlanDirectiveStatus, PlanExecutorPolicy, PlanHarnessSnapshot,
    PlanNodeResult, PlanNodeStatus, PlanRecoveryPolicySnapshot, ProviderName,
};
use std::time::Duration;
use tokio::sync::Notify;

#[tokio::test(flavor = "current_thread")]
async fn plan_scheduler_runs_disjoint_ready_leaves_concurrently_with_stable_worker_tools() {
    let probe = Arc::new(WorkerConcurrencyProbe::default());
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(CompletingPlanWorkerFactory {
        probe: Arc::clone(&probe),
    });
    let runtime = Runtime::builder(session_id("runtime-plan-scheduler-concurrency"))
        .coordinator_plan_tools()
        .plan_worker_factory(factory, 2)
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "execute parallel plan".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let update = runtime
        .update_plan(parallel_plan_input())
        .await
        .expect("plan definition succeeds");
    let mut events = runtime.subscribe_plan_events();
    runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot {
                write_scope: vec!["left".to_owned(), "right".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan is authorized");

    let final_snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            let completed = ["left", "right"].iter().all(|key| {
                snapshot
                    .nodes
                    .iter()
                    .find(|node| node.id == update.client_key_ids[*key])
                    .is_some_and(|node| node.status == PlanNodeStatus::Completed)
            });
            if completed {
                break snapshot;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("parallel workers should finish");

    assert!(probe.max_active.load(Ordering::Acquire) >= 2);
    assert_eq!(
        final_snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.outcome == Some(PlanAttemptOutcome::Completed))
            .count(),
        2
    );
    let requests = probe
        .requests
        .lock()
        .expect("request probe mutex is not poisoned");
    assert!(requests.len() >= 2, "each worker should issue one request");
    let expected_tools = ["report_plan_attempt", "report_plan_progress"];
    for request in requests.iter() {
        let names = request
            .tools()
            .iter()
            .map(|tool| tool.name().as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, expected_tools);
    }
    assert!(requests.iter().all(|request| {
        request
            .messages()
            .iter()
            .any(|message| format!("{message:?}").contains("plan_worker_context"))
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn worker_expansion_is_scheduled_recursively_by_the_root_scheduler() {
    let probe = Arc::new(RecursiveWorkerProbe::default());
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(RecursivePlanWorkerFactory {
        probe: Arc::clone(&probe),
    });
    let runtime = Runtime::builder(session_id("runtime-plan-scheduler-recursive"))
        .coordinator_plan_tools()
        .plan_worker_factory(factory, 2)
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "execute recursively expanded plan".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let update = runtime
        .update_plan(recursive_plan_input())
        .await
        .expect("recursive plan definition succeeds");
    let parent_id = update.client_key_ids["expandable"].clone();
    let mut events = runtime.subscribe_plan_events();
    runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot {
                write_scope: vec!["tree".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan is authorized");

    let final_snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            let parent_completed = snapshot
                .nodes
                .iter()
                .find(|node| node.id == parent_id)
                .is_some_and(|node| node.status == PlanNodeStatus::Completed);
            let completed_grandchildren = snapshot
                .nodes
                .iter()
                .filter(|node| node.parent_id.as_ref() == Some(&parent_id))
                .filter(|node| node.status == PlanNodeStatus::Completed)
                .count();
            if parent_completed && completed_grandchildren == 2 {
                break snapshot;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("recursive workers should finish");

    let parent_outcomes = final_snapshot
        .attempts
        .iter()
        .filter(|attempt| attempt.node_id == parent_id)
        .filter_map(|attempt| attempt.outcome)
        .collect::<Vec<_>>();
    assert_eq!(
        parent_outcomes,
        [
            PlanAttemptOutcome::Decomposed,
            PlanAttemptOutcome::Completed
        ]
    );
    assert!(probe.max_active.load(Ordering::Acquire) >= 2);
    assert_eq!(probe.parent_attempts.load(Ordering::Acquire), 2);
    assert!(
        probe
            .worker_depths
            .lock()
            .expect("worker depth mutex is not poisoned")
            .iter()
            .all(|depth| *depth == 1),
        "every recursive task remains a depth-one worker under the root scheduler"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn directive_waits_for_the_next_worker_provider_boundary_before_delivery() {
    let probe = Arc::new(DirectiveDeliveryProbe::default());
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(DirectivePlanWorkerFactory {
        probe: Arc::clone(&probe),
    });
    let runtime = Runtime::builder(session_id("runtime-plan-directive-boundary"))
        .coordinator_plan_tools()
        .plan_worker_factory(factory, 1)
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "test safe directive delivery".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let update = runtime
        .update_plan(single_worker_plan_input())
        .await
        .expect("plan definition succeeds");
    let worker_id = update.client_key_ids["worker"].clone();
    let mut events = runtime.subscribe_plan_events();
    runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot {
                write_scope: vec!["work".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan is authorized");

    probe.first_request_started.notified().await;
    let live = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    let attempt = live
        .attempts
        .iter()
        .find(|attempt| attempt.node_id == worker_id && attempt.outcome.is_none())
        .expect("worker has a live attempt");
    let lease = live
        .leases
        .iter()
        .find(|lease| lease.attempt_id == attempt.attempt_id)
        .expect("live attempt has a lease");
    let issued = runtime
        .inner
        .plan_controller
        .directive(
            ControlPlanAttemptInput {
                attempt_id: attempt.attempt_id.clone(),
                expected_lease_id: lease.lease_id.clone(),
                expected_node_revision: lease.node_revision,
                kind: PlanDirectiveKind::Converge,
                reason: "The current evidence is sufficient".to_owned(),
                instruction: Some("Return a bounded result now".to_owned()),
                constraints: Some(PlanDirectiveConstraints {
                    allow_decomposition: false,
                    ..PlanDirectiveConstraints::default()
                }),
                requested_output: vec!["terminal verification summary".to_owned()],
            },
            2_000,
        )
        .await
        .expect("directive commits while provider request is in flight");
    assert_eq!(issued.output.directive.status, PlanDirectiveStatus::Queued);
    assert!(!probe.second_request_saw_delivery.load(Ordering::Acquire));

    probe.release_first_request.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            if snapshot
                .nodes
                .iter()
                .find(|node| node.id == worker_id)
                .is_some_and(|node| node.status == PlanNodeStatus::Completed)
            {
                break;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("worker should finish after receiving the directive");

    assert!(probe.second_request_saw_delivery.load(Ordering::Acquire));
}

#[derive(Default)]
struct WorkerConcurrencyProbe {
    active: AtomicUsize,
    max_active: AtomicUsize,
    notify: Notify,
    requests: StdMutex<Vec<ModelRequest>>,
}

struct CompletingPlanWorkerFactory {
    probe: Arc<WorkerConcurrencyProbe>,
}

#[derive(Default)]
struct RecursiveWorkerProbe {
    parent_attempts: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
    notify: Notify,
    worker_depths: StdMutex<Vec<u8>>,
}

struct RecursivePlanWorkerFactory {
    probe: Arc<RecursiveWorkerProbe>,
}

#[derive(Default)]
struct DirectiveDeliveryProbe {
    first_request_started: Notify,
    release_first_request: Notify,
    second_request_saw_delivery: AtomicBool,
}

struct DirectivePlanWorkerFactory {
    probe: Arc<DirectiveDeliveryProbe>,
}

impl ChildRuntimeFactory for DirectivePlanWorkerFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let control = input
            .plan_worker_control
            .clone()
            .expect("scheduler worker carries plan control");
        let provider = DirectiveWorkerProvider {
            probe: Arc::clone(&self.probe),
            calls: AtomicUsize::new(0),
            lease_id: control.lease_id().clone(),
            node_revision: control.node_revision(),
        };
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(Arc::new(provider), model_name())
            .automatic_compaction(AutomaticCompactionConfig::disabled())
            .plan_worker_control(control)
            .build()
    }
}

struct DirectiveWorkerProvider {
    probe: Arc<DirectiveDeliveryProbe>,
    calls: AtomicUsize,
    lease_id: merry_core::PlanLeaseId,
    node_revision: u64,
}

impl ModelProvider for DirectiveWorkerProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| ProviderName::new("directive-plan-worker-test").expect("valid name"))
    }

    fn capabilities(&self) -> &ModelCapabilities {
        static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ModelCapabilities::new(true, true, false, true, Some(128_000), None)
                .expect("valid capabilities")
        })
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let call_index = self.calls.fetch_add(1, Ordering::AcqRel);
            let event = match call_index {
                0 => {
                    self.probe.first_request_started.notify_one();
                    self.probe.release_first_request.notified().await;
                    completed_event_with(
                        vec![ModelOutput::tool_call(report_progress_model_call(
                            &self.lease_id,
                            self.node_revision,
                        ))],
                        FinishReason::ToolCalls,
                    )
                }
                1 => {
                    let rendered = format!("{request:?}");
                    self.probe.second_request_saw_delivery.store(
                        rendered.contains("delivered")
                            && rendered.contains("Return a bounded result now"),
                        Ordering::Release,
                    );
                    completed_event_with(
                        vec![ModelOutput::tool_call(report_attempt_model_call(
                            &self.lease_id,
                            self.node_revision,
                            "directive applied at a safe provider boundary",
                        ))],
                        FinishReason::ToolCalls,
                    )
                }
                _ => completed_event(),
            };
            let stream: ModelEventStream = Box::pin(tokio_stream::iter(vec![Ok(event)]));
            Ok(stream)
        })
    }
}

impl ChildRuntimeFactory for RecursivePlanWorkerFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let control = input
            .plan_worker_control
            .clone()
            .expect("scheduler worker carries plan control");
        self.probe
            .worker_depths
            .lock()
            .expect("worker depth mutex is not poisoned")
            .push(input.depth);
        let task = input.task.task().to_owned();
        let action = if task.contains("Expand recursive branch") {
            if self.probe.parent_attempts.fetch_add(1, Ordering::AcqRel) == 0 {
                RecursiveWorkerAction::Decompose
            } else {
                RecursiveWorkerAction::Complete
            }
        } else {
            RecursiveWorkerAction::CompleteGrandchild
        };
        let provider = RecursiveWorkerProvider {
            probe: Arc::clone(&self.probe),
            calls: AtomicUsize::new(0),
            lease_id: control.lease_id().clone(),
            node_revision: control.node_revision(),
            conclusion: task,
            action,
        };
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(Arc::new(provider), model_name())
            .automatic_compaction(AutomaticCompactionConfig::disabled())
            .plan_worker_control(control)
            .build()
    }
}

#[derive(Clone, Copy)]
enum RecursiveWorkerAction {
    Decompose,
    Complete,
    CompleteGrandchild,
}

struct RecursiveWorkerProvider {
    probe: Arc<RecursiveWorkerProbe>,
    calls: AtomicUsize,
    lease_id: merry_core::PlanLeaseId,
    node_revision: u64,
    conclusion: String,
    action: RecursiveWorkerAction,
}

impl ModelProvider for RecursiveWorkerProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| ProviderName::new("recursive-plan-worker-test").expect("valid name"))
    }

    fn capabilities(&self) -> &ModelCapabilities {
        static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ModelCapabilities::new(true, true, false, true, Some(128_000), None)
                .expect("valid capabilities")
        })
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let call_index = self.calls.fetch_add(1, Ordering::AcqRel);
            let event = if call_index == 0 {
                if matches!(self.action, RecursiveWorkerAction::CompleteGrandchild) {
                    let active = self.probe.active.fetch_add(1, Ordering::AcqRel) + 1;
                    self.probe.max_active.fetch_max(active, Ordering::AcqRel);
                    if active >= 2 {
                        self.probe.notify.notify_waiters();
                    }
                    while self.probe.max_active.load(Ordering::Acquire) < 2 {
                        self.probe.notify.notified().await;
                    }
                    self.probe.active.fetch_sub(1, Ordering::AcqRel);
                }
                let call = match self.action {
                    RecursiveWorkerAction::Decompose => {
                        decomposition_model_call(&self.lease_id, self.node_revision)
                    }
                    RecursiveWorkerAction::Complete | RecursiveWorkerAction::CompleteGrandchild => {
                        report_attempt_model_call(
                            &self.lease_id,
                            self.node_revision,
                            &self.conclusion,
                        )
                    }
                };
                completed_event_with(vec![ModelOutput::tool_call(call)], FinishReason::ToolCalls)
            } else {
                completed_event()
            };
            let stream: ModelEventStream = Box::pin(tokio_stream::iter(vec![Ok(event)]));
            Ok(stream)
        })
    }
}

impl ChildRuntimeFactory for CompletingPlanWorkerFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let control = input
            .plan_worker_control
            .clone()
            .expect("scheduler worker carries plan control");
        let provider = CompletingWorkerProvider {
            probe: Arc::clone(&self.probe),
            calls: AtomicUsize::new(0),
            lease_id: control.lease_id().clone(),
            node_revision: control.node_revision(),
            conclusion: input.task.task().to_owned(),
        };
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(Arc::new(provider), model_name())
            .automatic_compaction(AutomaticCompactionConfig::disabled())
            .plan_worker_control(control)
            .build()
    }
}

struct CompletingWorkerProvider {
    probe: Arc<WorkerConcurrencyProbe>,
    calls: AtomicUsize,
    lease_id: merry_core::PlanLeaseId,
    node_revision: u64,
    conclusion: String,
}

impl ModelProvider for CompletingWorkerProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| ProviderName::new("plan-worker-test").expect("valid provider name"))
    }

    fn capabilities(&self) -> &ModelCapabilities {
        static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ModelCapabilities::new(true, true, false, true, Some(128_000), None)
                .expect("valid capabilities")
        })
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            self.probe
                .requests
                .lock()
                .expect("request probe mutex is not poisoned")
                .push(request);
            let call_index = self.calls.fetch_add(1, Ordering::AcqRel);
            let event = if call_index == 0 {
                let active = self.probe.active.fetch_add(1, Ordering::AcqRel) + 1;
                self.probe.max_active.fetch_max(active, Ordering::AcqRel);
                if active >= 2 {
                    self.probe.notify.notify_waiters();
                }
                while self.probe.max_active.load(Ordering::Acquire) < 2 {
                    self.probe.notify.notified().await;
                }
                self.probe.active.fetch_sub(1, Ordering::AcqRel);
                completed_event_with(
                    vec![ModelOutput::tool_call(report_attempt_model_call(
                        &self.lease_id,
                        self.node_revision,
                        &self.conclusion,
                    ))],
                    FinishReason::ToolCalls,
                )
            } else {
                completed_event()
            };
            let stream: ModelEventStream = Box::pin(tokio_stream::iter(vec![Ok(event)]));
            Ok(stream)
        })
    }
}

fn report_attempt_model_call(
    lease_id: &merry_core::PlanLeaseId,
    node_revision: u64,
    conclusion: &str,
) -> ModelToolCall {
    let input = ReportPlanAttemptInput {
        lease_id: lease_id.clone(),
        expected_node_revision: node_revision,
        outcome: PlanAttemptOutcome::Completed,
        result: Some(PlanNodeResult {
            conclusion: conclusion.to_owned(),
            evidence_refs: Vec::new(),
            artifact_refs: Vec::new(),
            changed_paths: Vec::new(),
            verification: vec!["offline worker verification".to_owned()],
            open_questions: Vec::new(),
        }),
        diagnostic: None,
        decomposition: None,
        acknowledged_directive_ids: Vec::new(),
        applied_directive_ids: Vec::new(),
    };
    ModelToolCall::new(
        ModelToolCallId::new(&format!("call-{}", lease_id.as_str())).expect("valid call id"),
        ToolName::new("report_plan_attempt").expect("valid tool name"),
        ToolArguments::try_from(serde_json::to_value(input).expect("report input serializes"))
            .expect("report arguments are an object"),
    )
}

fn decomposition_model_call(
    lease_id: &merry_core::PlanLeaseId,
    node_revision: u64,
) -> ModelToolCall {
    let input = ReportPlanAttemptInput {
        lease_id: lease_id.clone(),
        expected_node_revision: node_revision,
        outcome: PlanAttemptOutcome::Decomposed,
        result: None,
        diagnostic: None,
        decomposition: Some(PlanDecompositionInput {
            reason: "split independent recursive work".to_owned(),
            children: vec![
                worker_leaf("grand-left", "tree/left"),
                worker_leaf("grand-right", "tree/right"),
            ],
        }),
        acknowledged_directive_ids: Vec::new(),
        applied_directive_ids: Vec::new(),
    };
    ModelToolCall::new(
        ModelToolCallId::new(&format!("decompose-{}", lease_id.as_str())).expect("valid call id"),
        ToolName::new("report_plan_attempt").expect("valid tool name"),
        ToolArguments::try_from(serde_json::to_value(input).expect("report input serializes"))
            .expect("report arguments are an object"),
    )
}

fn report_progress_model_call(
    lease_id: &merry_core::PlanLeaseId,
    node_revision: u64,
) -> ModelToolCall {
    let input = ReportPlanProgressInput {
        lease_id: lease_id.clone(),
        expected_node_revision: node_revision,
        summary: "Initial provider request completed".to_owned(),
        evidence_refs: Vec::new(),
        artifact_refs: Vec::new(),
        next_action: Some("Inspect coordinator directives".to_owned()),
        checkpoint_ref: None,
        acknowledged_directive_ids: Vec::new(),
        applied_directive_ids: Vec::new(),
        request_coordinator_review: Some(false),
    };
    ModelToolCall::new(
        ModelToolCallId::new(&format!("progress-{}", lease_id.as_str())).expect("valid call id"),
        ToolName::new("report_plan_progress").expect("valid tool name"),
        ToolArguments::try_from(serde_json::to_value(input).expect("progress input serializes"))
            .expect("progress arguments are an object"),
    )
}

fn parallel_plan_input() -> UpdatePlanInput {
    let mut root_harness = PlanHarnessSnapshot::default();
    root_harness.write_scope = vec!["left".to_owned(), "right".to_owned()];
    UpdatePlanInput {
        reason: "define disjoint parallel work".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(2),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Verify both branches".to_owned(),
                acceptance: vec!["both branches complete".to_owned()],
                executor_policy: PlanExecutorPolicy::Local,
                harness: root_harness,
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: vec![worker_leaf("left", "left"), worker_leaf("right", "right")],
            },
        },
    }
}

fn recursive_plan_input() -> UpdatePlanInput {
    let mut root_harness = PlanHarnessSnapshot::default();
    root_harness.write_scope = vec!["tree".to_owned()];
    let mut expandable_harness = PlanHarnessSnapshot::default();
    expandable_harness.write_scope = vec!["tree".to_owned()];
    UpdatePlanInput {
        reason: "define lazily expandable work".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(2),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Verify recursive work".to_owned(),
                acceptance: vec!["expanded branch is verified".to_owned()],
                executor_policy: PlanExecutorPolicy::Local,
                harness: root_harness,
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: vec![PlanNodeInput {
                    id: None,
                    client_key: Some("expandable".to_owned()),
                    objective: "Expand recursive branch".to_owned(),
                    acceptance: vec!["both direct descendants are synthesized".to_owned()],
                    executor_policy: PlanExecutorPolicy::Delegate,
                    harness: expandable_harness,
                    recovery_policy: PlanRecoveryPolicySnapshot::default(),
                    depends_on: Vec::new(),
                    children: Vec::new(),
                }],
            },
        },
    }
}

fn single_worker_plan_input() -> UpdatePlanInput {
    let mut root_harness = PlanHarnessSnapshot::default();
    root_harness.write_scope = vec!["work".to_owned()];
    UpdatePlanInput {
        reason: "define one steerable worker".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(1),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Verify steerable work".to_owned(),
                acceptance: vec!["worker result is verified".to_owned()],
                executor_policy: PlanExecutorPolicy::Local,
                harness: root_harness,
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: vec![worker_leaf("worker", "work")],
            },
        },
    }
}

fn worker_leaf(client_key: &str, write_scope: &str) -> PlanNodeInput {
    let mut harness = PlanHarnessSnapshot::default();
    harness.write_scope = vec![write_scope.to_owned()];
    PlanNodeInput {
        id: None,
        client_key: Some(client_key.to_owned()),
        objective: format!("Complete {client_key} branch"),
        acceptance: vec![format!("{client_key} verified")],
        executor_policy: PlanExecutorPolicy::Delegate,
        harness,
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children: Vec::new(),
    }
}
