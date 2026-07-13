use super::*;
use crate::plan::{
    BeginPlanInput, ControlPlanAttemptInput, PlanChangeInput, PlanDecompositionInput,
    PlanExecutionIntent, PlanNodeInput, ReportPlanAttemptInput, ReportPlanProgressInput,
    UpdatePlanInput,
};
use crate::{ChildRuntimeFactory, ChildRuntimeInput, FileSessionStore};
use merry_core::{
    PlanAttemptOutcome, PlanCapabilityEnvelopeSnapshot, PlanDirectiveConstraints,
    PlanDirectiveKind, PlanDirectiveStatus, PlanExecutorPolicy, PlanHarnessSnapshot,
    PlanNodeResult, PlanNodeStatus, PlanPhase, PlanRecoveryPolicySnapshot, ProviderName,
};
use std::{sync::Condvar, time::Duration};
use tokio::sync::Notify;

#[tokio::test(flavor = "current_thread")]
async fn plan_scheduler_runs_disjoint_ready_leaves_concurrently_with_stable_subagent_tools() {
    let probe = Arc::new(SubagentConcurrencyProbe::default());
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(CompletingPlanSubagentFactory {
        probe: Arc::clone(&probe),
    });
    let runtime = Runtime::builder(session_id("runtime-plan-scheduler-concurrency"))
        .coordinator_plan_tools()
        .plan_subagent_factory(factory, 2)
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
    .expect("parallel subagents should finish");

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
    assert!(
        requests.len() >= 2,
        "each subagent should issue one request"
    );
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
            .any(|message| format!("{message:?}").contains("plan_subagent_context"))
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_expansion_is_scheduled_recursively_by_the_root_scheduler() {
    let probe = Arc::new(RecursiveSubagentProbe::default());
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(RecursivePlanSubagentFactory {
        probe: Arc::clone(&probe),
    });
    let runtime = Runtime::builder(session_id("runtime-plan-scheduler-recursive"))
        .coordinator_plan_tools()
        .plan_subagent_factory(factory, 2)
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
    .expect("recursive subagents should finish");

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
            .subagent_depths
            .lock()
            .expect("subagent depth mutex is not poisoned")
            .iter()
            .all(|depth| *depth == 1),
        "every recursive task remains a depth-one subagent under the root scheduler"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn directive_waits_for_the_next_subagent_provider_boundary_before_delivery() {
    let probe = Arc::new(DirectiveDeliveryProbe::default());
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(DirectivePlanSubagentFactory {
        probe: Arc::clone(&probe),
    });
    let runtime = Runtime::builder(session_id("runtime-plan-directive-boundary"))
        .coordinator_plan_tools()
        .plan_subagent_factory(factory, 1)
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
        .update_plan(single_subagent_plan_input())
        .await
        .expect("plan definition succeeds");
    let subagent_id = update.client_key_ids["subagent"].clone();
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
        .find(|attempt| attempt.node_id == subagent_id && attempt.outcome.is_none())
        .expect("subagent has a live attempt");
    let _lease = live
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
                .find(|node| node.id == subagent_id)
                .is_some_and(|node| node.status == PlanNodeStatus::Completed)
            {
                break;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("subagent should finish after receiving the directive");

    assert!(probe.second_request_saw_delivery.load(Ordering::Acquire));
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_plan_stops_live_subagent_and_commits_cancelled_attempt() {
    let probe = Arc::new(CancellationProbe::default());
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(CancellablePlanSubagentFactory {
        probe: Arc::clone(&probe),
    });
    let runtime = Runtime::builder(session_id("runtime-plan-cancellation"))
        .coordinator_plan_tools()
        .plan_subagent_factory(factory, 1)
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "cancel live plan work".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(single_subagent_plan_input())
        .await
        .expect("plan definition succeeds");
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
    probe.started.notified().await;

    runtime
        .cancel_plan("user cancelled live work")
        .await
        .expect("cancellation request commits");
    let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            if snapshot.phase == PlanPhase::Cancelled {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cooperative subagent cancellation should settle");
    assert_eq!(snapshot.attempts.len(), 1);
    assert_eq!(
        snapshot.attempts[0].outcome,
        Some(PlanAttemptOutcome::Cancelled)
    );
    assert_eq!(
        snapshot.leases[0].status,
        merry_core::PlanLeaseStatus::Cancelled
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_during_subagent_construction_prevents_child_execution() {
    let probe = Arc::new(BuildCancellationProbe::default());
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(BlockingBuildPlanSubagentFactory {
        probe: Arc::clone(&probe),
        fail_after_release: false,
    });
    let runtime = Runtime::builder(session_id("runtime-plan-cancel-during-child-build"))
        .coordinator_plan_tools()
        .plan_subagent_factory(factory, 1)
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "cancel while the delegated child is being built".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(single_subagent_plan_input())
        .await
        .expect("plan definition succeeds");
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
    probe.build_started.notified().await;

    runtime
        .cancel_plan("user cancelled during child construction")
        .await
        .expect("cancellation request commits");
    probe.release_build();

    let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            if snapshot.phase == PlanPhase::Cancelled {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled startup should settle");

    assert_eq!(snapshot.attempts.len(), 1);
    assert_eq!(
        snapshot.attempts[0].outcome,
        Some(PlanAttemptOutcome::Cancelled)
    );
    assert_eq!(probe.provider_calls.load(Ordering::Acquire), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_during_failed_subagent_construction_remains_cancelled() {
    let probe = Arc::new(BuildCancellationProbe::default());
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(BlockingBuildPlanSubagentFactory {
        probe: Arc::clone(&probe),
        fail_after_release: true,
    });
    let runtime = Runtime::builder(session_id("runtime-plan-cancel-during-failed-child-build"))
        .coordinator_plan_tools()
        .plan_subagent_factory(factory, 1)
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "cancel while a failing delegated child is being built".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(single_subagent_plan_input())
        .await
        .expect("plan definition succeeds");
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
    probe.build_started.notified().await;

    runtime
        .cancel_plan("user cancelled during failed child construction")
        .await
        .expect("cancellation request commits");
    probe.release_build();

    let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            if snapshot.phase == PlanPhase::Cancelled {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled failed startup should settle");

    assert_eq!(snapshot.attempts.len(), 1);
    assert_eq!(
        snapshot.attempts[0].outcome,
        Some(PlanAttemptOutcome::Cancelled)
    );
    assert_eq!(probe.provider_calls.load(Ordering::Acquire), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_the_last_runtime_handle_stops_scheduler_without_post_drop_plan_writes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let probe = Arc::new(CancellationProbe::default());
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(CancellablePlanSubagentFactory {
        probe: Arc::clone(&probe),
    });
    let session_id = session_id("runtime-plan-scheduler-drop");
    let runtime = Runtime::builder(session_id.clone())
        .coordinator_plan_tools()
        .plan_subagent_factory(factory, 1)
        .session_store(store.clone())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "prove scheduler shutdown ownership".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(single_subagent_plan_input())
        .await
        .expect("plan definition succeeds");
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
    probe.started.notified().await;
    let attempt_id = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists")
        .attempts[0]
        .attempt_id
        .clone();
    assert_eq!(
        Arc::strong_count(&runtime.inner),
        1,
        "scheduler tasks must not retain the parent RuntimeInner"
    );

    drop(runtime);
    tokio::time::timeout(Duration::from_secs(2), probe.cancelled.notified())
        .await
        .expect("dropping the runtime cancels the live child");
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    let loaded = Runtime::builder(session_id)
        .load_session_from_store(store)
        .await
        .expect("persisted session loads")
        .build()
        .expect("loaded runtime builds without starting its scheduler");
    let snapshot = loaded
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    let attempt = snapshot
        .attempts
        .iter()
        .find(|attempt| attempt.attempt_id == attempt_id)
        .expect("persisted attempt remains");
    assert_eq!(attempt.outcome, None);
    assert_eq!(snapshot.attempts.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn missing_subagent_attempt_report_is_retried_without_hanging() {
    let probe = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(MissingReportPlanSubagentFactory {
        calls: Arc::clone(&probe),
    });
    let runtime = Runtime::builder(session_id("runtime-plan-missing-subagent-report"))
        .coordinator_plan_tools()
        .plan_subagent_factory(factory, 1)
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "verify missing subagent report recovery".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(single_subagent_plan_input())
        .await
        .expect("plan definition succeeds");
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

    let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            if snapshot.phase == PlanPhase::Blocked {
                break snapshot;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("bounded missing-report retries should settle");

    assert_eq!(snapshot.attempts.len(), 2);
    assert_eq!(snapshot.leases.len(), 2);
    assert!(snapshot.attempts.iter().all(|attempt| {
        attempt.outcome == Some(PlanAttemptOutcome::TransientFailure)
            && attempt
                .diagnostic
                .as_ref()
                .is_some_and(|diagnostic| diagnostic.code() == "missing_attempt_report")
    }));
    assert_eq!(probe.load(Ordering::Acquire), 2);
}

#[derive(Default)]
struct SubagentConcurrencyProbe {
    active: AtomicUsize,
    max_active: AtomicUsize,
    notify: Notify,
    requests: StdMutex<Vec<ModelRequest>>,
}

struct CompletingPlanSubagentFactory {
    probe: Arc<SubagentConcurrencyProbe>,
}

#[derive(Default)]
struct RecursiveSubagentProbe {
    parent_attempts: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
    notify: Notify,
    subagent_depths: StdMutex<Vec<u8>>,
}

struct RecursivePlanSubagentFactory {
    probe: Arc<RecursiveSubagentProbe>,
}

#[derive(Default)]
struct DirectiveDeliveryProbe {
    first_request_started: Notify,
    release_first_request: Notify,
    second_request_saw_delivery: AtomicBool,
}

struct DirectivePlanSubagentFactory {
    probe: Arc<DirectiveDeliveryProbe>,
}

#[derive(Default)]
struct CancellationProbe {
    started: Notify,
    cancelled: Notify,
}

struct CancellablePlanSubagentFactory {
    probe: Arc<CancellationProbe>,
}

#[derive(Default)]
struct BuildCancellationProbe {
    build_started: Notify,
    build_released: StdMutex<bool>,
    build_release: Condvar,
    provider_calls: AtomicUsize,
}

impl BuildCancellationProbe {
    fn wait_for_build_release(&self) {
        let mut released = self
            .build_released
            .lock()
            .expect("build release mutex is not poisoned");
        while !*released {
            released = self
                .build_release
                .wait(released)
                .expect("build release mutex is not poisoned");
        }
    }

    fn release_build(&self) {
        *self
            .build_released
            .lock()
            .expect("build release mutex is not poisoned") = true;
        self.build_release.notify_all();
    }
}

struct BlockingBuildPlanSubagentFactory {
    probe: Arc<BuildCancellationProbe>,
    fail_after_release: bool,
}

struct MissingReportPlanSubagentFactory {
    calls: Arc<AtomicUsize>,
}

impl ChildRuntimeFactory for BlockingBuildPlanSubagentFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        self.probe.build_started.notify_one();
        self.probe.wait_for_build_release();
        if self.fail_after_release {
            return Err(RuntimeError::InvalidStepInput {
                reason: "test subagent construction failure",
            });
        }
        let control = input
            .plan_subagent_control
            .expect("scheduler subagent carries plan control");
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(
                Arc::new(CountingCompletedSubagentProvider {
                    calls: Arc::clone(&self.probe),
                }),
                model_name(),
            )
            .automatic_compaction(AutomaticCompactionConfig::disabled())
            .plan_subagent_control(control)
            .build()
    }
}

struct CountingCompletedSubagentProvider {
    calls: Arc<BuildCancellationProbe>,
}

impl ModelProvider for CountingCompletedSubagentProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| {
            ProviderName::new("cancelled-build-plan-subagent-test").expect("valid name")
        })
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
            self.calls.provider_calls.fetch_add(1, Ordering::AcqRel);
            let stream: ModelEventStream =
                Box::pin(tokio_stream::iter(vec![Ok(completed_event())]));
            Ok(stream)
        })
    }
}

impl ChildRuntimeFactory for MissingReportPlanSubagentFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let control = input
            .plan_subagent_control
            .expect("scheduler subagent carries plan control");
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(
                Arc::new(MissingReportSubagentProvider {
                    calls: Arc::clone(&self.calls),
                }),
                model_name(),
            )
            .automatic_compaction(AutomaticCompactionConfig::disabled())
            .plan_subagent_control(control)
            .build()
    }
}

struct MissingReportSubagentProvider {
    calls: Arc<AtomicUsize>,
}

impl ModelProvider for MissingReportSubagentProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| {
            ProviderName::new("missing-report-plan-subagent-test").expect("valid name")
        })
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
            self.calls.fetch_add(1, Ordering::AcqRel);
            let stream: ModelEventStream =
                Box::pin(tokio_stream::iter(vec![Ok(completed_event())]));
            Ok(stream)
        })
    }
}

impl ChildRuntimeFactory for CancellablePlanSubagentFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let control = input
            .plan_subagent_control
            .expect("scheduler subagent carries plan control");
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(
                Arc::new(CancellableSubagentProvider {
                    probe: Arc::clone(&self.probe),
                }),
                model_name(),
            )
            .automatic_compaction(AutomaticCompactionConfig::disabled())
            .plan_subagent_control(control)
            .build()
    }
}

struct CancellableSubagentProvider {
    probe: Arc<CancellationProbe>,
}

struct CancellationDropGuard(Arc<CancellationProbe>);

impl Drop for CancellationDropGuard {
    fn drop(&mut self) {
        self.0.cancelled.notify_one();
    }
}

impl ModelProvider for CancellableSubagentProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| {
            ProviderName::new("cancellable-plan-subagent-test").expect("valid name")
        })
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
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let _drop_guard = CancellationDropGuard(Arc::clone(&self.probe));
            self.probe.started.notify_one();
            context.cancellation_token().cancelled().await;
            Err(ModelError::Cancelled)
        })
    }
}

impl ChildRuntimeFactory for DirectivePlanSubagentFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let control = input
            .plan_subagent_control
            .clone()
            .expect("scheduler subagent carries plan control");
        let provider = DirectiveSubagentProvider {
            probe: Arc::clone(&self.probe),
            calls: AtomicUsize::new(0),
            lease_id: control.lease_id().clone(),
        };
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(Arc::new(provider), model_name())
            .automatic_compaction(AutomaticCompactionConfig::disabled())
            .plan_subagent_control(control)
            .build()
    }
}

struct DirectiveSubagentProvider {
    probe: Arc<DirectiveDeliveryProbe>,
    calls: AtomicUsize,
    lease_id: merry_core::PlanLeaseId,
}

impl ModelProvider for DirectiveSubagentProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| ProviderName::new("directive-plan-subagent-test").expect("valid name"))
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

impl ChildRuntimeFactory for RecursivePlanSubagentFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let control = input
            .plan_subagent_control
            .clone()
            .expect("scheduler subagent carries plan control");
        self.probe
            .subagent_depths
            .lock()
            .expect("subagent depth mutex is not poisoned")
            .push(input.depth);
        let task = input.task.task().to_owned();
        let action = if task.contains("Expand recursive branch") {
            if self.probe.parent_attempts.fetch_add(1, Ordering::AcqRel) == 0 {
                RecursiveSubagentAction::Decompose
            } else {
                RecursiveSubagentAction::Complete
            }
        } else {
            RecursiveSubagentAction::CompleteGrandchild
        };
        let provider = RecursiveSubagentProvider {
            probe: Arc::clone(&self.probe),
            calls: AtomicUsize::new(0),
            lease_id: control.lease_id().clone(),
            conclusion: task,
            action,
        };
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(Arc::new(provider), model_name())
            .automatic_compaction(AutomaticCompactionConfig::disabled())
            .plan_subagent_control(control)
            .build()
    }
}

#[derive(Clone, Copy)]
enum RecursiveSubagentAction {
    Decompose,
    Complete,
    CompleteGrandchild,
}

struct RecursiveSubagentProvider {
    probe: Arc<RecursiveSubagentProbe>,
    calls: AtomicUsize,
    lease_id: merry_core::PlanLeaseId,
    conclusion: String,
    action: RecursiveSubagentAction,
}

impl ModelProvider for RecursiveSubagentProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| ProviderName::new("recursive-plan-subagent-test").expect("valid name"))
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
                if matches!(self.action, RecursiveSubagentAction::CompleteGrandchild) {
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
                    RecursiveSubagentAction::Decompose => decomposition_model_call(&self.lease_id),
                    RecursiveSubagentAction::Complete
                    | RecursiveSubagentAction::CompleteGrandchild => {
                        report_attempt_model_call(&self.lease_id, &self.conclusion)
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

impl ChildRuntimeFactory for CompletingPlanSubagentFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
        let control = input
            .plan_subagent_control
            .clone()
            .expect("scheduler subagent carries plan control");
        let provider = CompletingSubagentProvider {
            probe: Arc::clone(&self.probe),
            calls: AtomicUsize::new(0),
            lease_id: control.lease_id().clone(),
            conclusion: input.task.task().to_owned(),
        };
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(Arc::new(provider), model_name())
            .automatic_compaction(AutomaticCompactionConfig::disabled())
            .plan_subagent_control(control)
            .build()
    }
}

struct CompletingSubagentProvider {
    probe: Arc<SubagentConcurrencyProbe>,
    calls: AtomicUsize,
    lease_id: merry_core::PlanLeaseId,
    conclusion: String,
}

impl ModelProvider for CompletingSubagentProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| ProviderName::new("plan-subagent-test").expect("valid provider name"))
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
    conclusion: &str,
) -> ModelToolCall {
    let input = ReportPlanAttemptInput {
        outcome: PlanAttemptOutcome::Completed,
        result: Some(PlanNodeResult {
            conclusion: conclusion.to_owned(),
            evidence_refs: Vec::new(),
            artifact_refs: Vec::new(),
            changed_paths: Vec::new(),
            verification: vec!["offline subagent verification".to_owned()],
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

fn decomposition_model_call(lease_id: &merry_core::PlanLeaseId) -> ModelToolCall {
    let input = ReportPlanAttemptInput {
        outcome: PlanAttemptOutcome::Decomposed,
        result: None,
        diagnostic: None,
        decomposition: Some(PlanDecompositionInput {
            reason: "split independent recursive work".to_owned(),
            children: vec![
                subagent_leaf("grand-left", "tree/left"),
                subagent_leaf("grand-right", "tree/right"),
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

fn report_progress_model_call(lease_id: &merry_core::PlanLeaseId) -> ModelToolCall {
    let input = ReportPlanProgressInput {
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

pub(super) fn parallel_plan_input() -> UpdatePlanInput {
    let root_harness = PlanHarnessSnapshot {
        write_scope: vec!["left".to_owned(), "right".to_owned()],
        ..PlanHarnessSnapshot::default()
    };
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
                children: vec![
                    subagent_leaf("left", "left"),
                    subagent_leaf("right", "right"),
                ],
            },
        },
    }
}

fn recursive_plan_input() -> UpdatePlanInput {
    let root_harness = PlanHarnessSnapshot {
        write_scope: vec!["tree".to_owned()],
        ..PlanHarnessSnapshot::default()
    };
    let expandable_harness = PlanHarnessSnapshot {
        write_scope: vec!["tree".to_owned()],
        ..PlanHarnessSnapshot::default()
    };
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

fn single_subagent_plan_input() -> UpdatePlanInput {
    let root_harness = PlanHarnessSnapshot {
        write_scope: vec!["work".to_owned()],
        ..PlanHarnessSnapshot::default()
    };
    UpdatePlanInput {
        reason: "define one steerable subagent".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(1),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Verify steerable work".to_owned(),
                acceptance: vec!["subagent result is verified".to_owned()],
                executor_policy: PlanExecutorPolicy::Local,
                harness: root_harness,
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: vec![subagent_leaf("subagent", "work")],
            },
        },
    }
}

fn subagent_leaf(client_key: &str, write_scope: &str) -> PlanNodeInput {
    let harness = PlanHarnessSnapshot {
        write_scope: vec![write_scope.to_owned()],
        ..PlanHarnessSnapshot::default()
    };
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
