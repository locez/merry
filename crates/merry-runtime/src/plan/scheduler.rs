use super::{PlanController, PlanWorkerControl, execution::PlanAttemptActor, unix_time_ms};
use crate::{
    AgentLoopConfig, AgentLoopStatus, ChildRuntimeFactory, ChildRuntimeInput, ChildWorkspaceScope,
    Runtime, StepContext, StepInput, SubagentTaskSpec, TaskAnchor,
};
use merry_core::{
    ErrorInfo, PlanAttemptId, PlanAttemptOutcome, PlanLeaseStatus, PlanNodeId, PlanNodeResult,
    PlanNodeSnapshot, PlanNodeStatus, PlanPhase, PlanSchedulerStatus, PlanSnapshot, SessionId,
};
use merry_llm::{GenerationConfig, ReasoningEffort};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct PlanScheduler {
    inner: Arc<PlanSchedulerInner>,
}

struct PlanSchedulerInner {
    controller: PlanController,
    factory: Arc<dyn ChildRuntimeFactory>,
    max_worker_threads: usize,
    started: AtomicBool,
    start_guard: StdMutex<()>,
    reconcile_guard: Mutex<()>,
    running: Mutex<BTreeMap<PlanAttemptId, RunningWorker>>,
    notify: Notify,
    cancellation: CancellationToken,
}

struct RunningWorker {
    write_scope: Vec<String>,
    cancellation: CancellationToken,
}

impl PlanScheduler {
    pub(crate) fn new(
        controller: PlanController,
        factory: Arc<dyn ChildRuntimeFactory>,
        max_worker_threads: usize,
    ) -> Self {
        Self {
            inner: Arc::new(PlanSchedulerInner {
                controller,
                factory,
                max_worker_threads: max_worker_threads.max(1),
                started: AtomicBool::new(false),
                start_guard: StdMutex::new(()),
                reconcile_guard: Mutex::new(()),
                running: Mutex::new(BTreeMap::new()),
                notify: Notify::new(),
                cancellation: CancellationToken::new(),
            }),
        }
    }

    pub(crate) fn ensure_started(&self) {
        if self.inner.started.load(Ordering::Acquire) {
            return;
        }
        let _guard = self
            .inner
            .start_guard
            .lock()
            .expect("plan scheduler start mutex is not poisoned");
        if self.inner.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let scheduler = self.clone();
        let mut events = self.inner.controller.subscribe();
        tokio::spawn(async move {
            let _ = scheduler
                .inner
                .controller
                .recover_live_leases_after_resume(unix_time_ms())
                .await;
            scheduler.reconcile().await;
            let mut recovery_interval = tokio::time::interval(std::time::Duration::from_secs(1));
            recovery_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            recovery_interval.tick().await;
            loop {
                tokio::select! {
                    _ = scheduler.inner.cancellation.cancelled() => break,
                    event = events.recv() => {
                        match event {
                            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                scheduler.reconcile().await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    _ = scheduler.inner.notify.notified() => {
                        scheduler.reconcile().await;
                    }
                    _ = recovery_interval.tick() => {
                        let _ = scheduler
                            .inner
                            .controller
                            .recover_expired_leases(unix_time_ms())
                            .await;
                        scheduler.reconcile().await;
                    }
                }
            }
        });
    }

    pub(crate) async fn reconcile(&self) {
        let _guard = self.inner.reconcile_guard.lock().await;
        let Ok(Some(snapshot)) = self.inner.controller.snapshot().await else {
            return;
        };
        if snapshot.phase != PlanPhase::Executing
            || snapshot.scheduler_status != PlanSchedulerStatus::Active
        {
            return;
        }
        let live_attempts = snapshot
            .leases
            .iter()
            .filter(|lease| lease.status == PlanLeaseStatus::Live)
            .map(|lease| lease.attempt_id.clone())
            .collect::<BTreeSet<_>>();
        {
            let mut running = self.inner.running.lock().await;
            running.retain(|attempt_id, worker| {
                let keep = live_attempts.contains(attempt_id);
                if !keep {
                    worker.cancellation.cancel();
                }
                keep
            });
        }
        let capacity = self.effective_capacity(&snapshot);
        let running_scopes = {
            let running = self.inner.running.lock().await;
            if running.len() >= capacity {
                return;
            }
            running
                .values()
                .map(|worker| worker.write_scope.clone())
                .collect::<Vec<_>>()
        };
        let mut selected_scopes = running_scopes;
        let available = capacity.saturating_sub(selected_scopes.len());
        let mut selected = Vec::new();
        for node_id in ready_node_ids(&snapshot) {
            if selected.len() == available {
                break;
            }
            let Some(node) = snapshot.nodes.iter().find(|node| node.id == node_id) else {
                continue;
            };
            if node.executor_policy == merry_core::PlanExecutorPolicy::Local {
                continue;
            }
            if selected_scopes
                .iter()
                .any(|scope| write_scopes_overlap(scope, &node.harness.write_scope))
            {
                continue;
            }
            selected_scopes.push(node.harness.write_scope.clone());
            selected.push(node.clone());
        }
        for node in selected {
            self.start_worker(snapshot.plan_id.clone(), node).await;
        }
    }

    pub(crate) async fn cancel_running_workers(&self) {
        let tokens = self
            .inner
            .running
            .lock()
            .await
            .values()
            .map(|worker| worker.cancellation.clone())
            .collect::<Vec<_>>();
        for token in tokens {
            token.cancel();
        }
    }

    fn effective_capacity(&self, snapshot: &PlanSnapshot) -> usize {
        self.inner
            .max_worker_threads
            .min(snapshot.resource_policy_snapshot.max_concurrency)
            .min(
                snapshot
                    .max_concurrency_hint
                    .unwrap_or(snapshot.resource_policy_snapshot.max_concurrency),
            )
            .max(1)
    }

    async fn start_worker(&self, plan_id: merry_core::PlanId, node: PlanNodeSnapshot) {
        let child_session_id = SessionId::random();
        let actor = PlanAttemptActor {
            executor_session_id: child_session_id.clone(),
        };
        let started = match self
            .inner
            .controller
            .start_attempt(node.id.clone(), actor.clone(), unix_time_ms())
            .await
        {
            Ok(committed) => committed.output,
            Err(_) => return,
        };
        let control = PlanWorkerControl::new(
            self.inner.controller.clone(),
            plan_id,
            node.id.clone(),
            started.lease.node_revision,
            started.attempt.attempt_id.clone(),
            started.lease.lease_id.clone(),
            child_session_id.clone(),
        );
        let (task, generation_config) = match worker_task(&node) {
            Ok(parts) => parts,
            Err(message) => {
                self.fail_started_attempt(
                    &control,
                    "plan_worker_contract_invalid",
                    message,
                    PlanAttemptOutcome::SemanticFailure,
                )
                .await;
                return;
            }
        };
        let task_anchor = match TaskAnchor::new(node.objective.clone()) {
            Ok(anchor) => anchor,
            Err(error) => {
                self.fail_started_attempt(
                    &control,
                    "plan_worker_anchor_invalid",
                    error.to_string(),
                    PlanAttemptOutcome::SemanticFailure,
                )
                .await;
                return;
            }
        };
        let runtime = match self.inner.factory.build_child(ChildRuntimeInput {
            session_id: child_session_id,
            task_anchor,
            allowed_tools: task.allowed_tools().to_vec(),
            workspace_scope: ChildWorkspaceScope::from_task(&task),
            task,
            depth: 1,
            generation_config: generation_config.clone(),
            plan_worker_control: Some(control.clone()),
        }) {
            Ok(runtime) => runtime,
            Err(error) => {
                self.fail_started_attempt(
                    &control,
                    "plan_worker_factory_failed",
                    error.to_string(),
                    PlanAttemptOutcome::TransientFailure,
                )
                .await;
                return;
            }
        };
        let cancellation = self.inner.cancellation.child_token();
        self.inner.running.lock().await.insert(
            started.attempt.attempt_id.clone(),
            RunningWorker {
                write_scope: node.harness.write_scope,
                cancellation: cancellation.clone(),
            },
        );
        let scheduler = self.clone();
        tokio::spawn(async move {
            scheduler
                .run_worker(runtime, control, generation_config, cancellation)
                .await;
        });
    }

    async fn run_worker(
        &self,
        runtime: Runtime,
        control: PlanWorkerControl,
        generation_config: GenerationConfig,
        cancellation: CancellationToken,
    ) {
        let input_text = match control.snapshot().await {
            Ok(snapshot) => worker_initial_prompt(&snapshot, control.node_id()),
            Err(error) => {
                self.fail_started_attempt(
                    &control,
                    "plan_worker_snapshot_failed",
                    error.to_string(),
                    PlanAttemptOutcome::TransientFailure,
                )
                .await;
                self.worker_finished(control.attempt_id()).await;
                return;
            }
        };
        let input = match StepInput::user_text(&input_text) {
            Ok(input) => input,
            Err(error) => {
                self.fail_started_attempt(
                    &control,
                    "plan_worker_input_invalid",
                    error.to_string(),
                    PlanAttemptOutcome::SemanticFailure,
                )
                .await;
                self.worker_finished(control.attempt_id()).await;
                return;
            }
        };
        let config = AgentLoopConfig::new(usize::MAX)
            .expect("effectively unbounded worker loop has a non-zero turn limit");
        let mut loop_future = Box::pin(runtime.run_agent_loop(
            input,
            StepContext::new(cancellation.clone()).with_generation_config(generation_config),
            config,
        ));
        let heartbeat_interval_ms = control
            .snapshot()
            .await
            .map(|snapshot| {
                snapshot
                    .resource_policy_snapshot
                    .worker_heartbeat_interval_ms
            })
            .unwrap_or(10_000)
            .max(1);
        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(heartbeat_interval_ms));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let loop_result = loop {
            tokio::select! {
                result = &mut loop_future => break result,
                _ = interval.tick() => {
                    let _ = control.heartbeat(unix_time_ms(), true, false).await;
                }
            }
        };
        let attempt_is_terminal = control
            .snapshot()
            .await
            .ok()
            .and_then(|snapshot| {
                snapshot
                    .attempts
                    .iter()
                    .find(|attempt| attempt.attempt_id == *control.attempt_id())
                    .and_then(|attempt| attempt.outcome)
            })
            .is_some();
        if !attempt_is_terminal {
            match loop_result {
                Ok(result) => match result.status() {
                    AgentLoopStatus::Completed => {
                        self.fail_started_attempt(
                            &control,
                            "missing_attempt_report",
                            "worker completed without report_plan_attempt",
                            PlanAttemptOutcome::SemanticFailure,
                        )
                        .await;
                    }
                    AgentLoopStatus::Failed { diagnostic } => {
                        self.fail_started_attempt(
                            &control,
                            diagnostic.code(),
                            diagnostic.message(),
                            PlanAttemptOutcome::TransientFailure,
                        )
                        .await;
                    }
                    AgentLoopStatus::Cancelled { diagnostic } => {
                        let draining = control.snapshot().await.is_ok_and(|snapshot| {
                            snapshot.scheduler_status == PlanSchedulerStatus::Draining
                        });
                        if draining {
                            let _ = control
                                .cancel_attempt(diagnostic.message(), unix_time_ms())
                                .await;
                        } else {
                            self.fail_started_attempt(
                                &control,
                                diagnostic.code(),
                                diagnostic.message(),
                                PlanAttemptOutcome::TransientFailure,
                            )
                            .await;
                        }
                    }
                    AgentLoopStatus::Blocked { reason } => {
                        self.fail_started_attempt(
                            &control,
                            "plan_worker_loop_blocked",
                            format!("{reason:?}"),
                            PlanAttemptOutcome::SemanticFailure,
                        )
                        .await;
                    }
                },
                Err(error) => {
                    self.fail_started_attempt(
                        &control,
                        "plan_worker_runtime_error",
                        error.to_string(),
                        PlanAttemptOutcome::TransientFailure,
                    )
                    .await;
                }
            }
        }
        self.worker_finished(control.attempt_id()).await;
    }

    async fn fail_started_attempt(
        &self,
        control: &PlanWorkerControl,
        code: &str,
        message: impl Into<String>,
        outcome: PlanAttemptOutcome,
    ) {
        let message = message.into();
        let Ok(diagnostic) = ErrorInfo::new(code, &message) else {
            return;
        };
        let _ = control
            .report_attempt(
                super::ReportPlanAttemptInput {
                    lease_id: control.lease_id().clone(),
                    expected_node_revision: control.node_revision(),
                    outcome,
                    result: (outcome == PlanAttemptOutcome::SemanticFailure).then(|| {
                        PlanNodeResult {
                            conclusion: message,
                            evidence_refs: Vec::new(),
                            artifact_refs: Vec::new(),
                            changed_paths: Vec::new(),
                            verification: Vec::new(),
                            open_questions: Vec::new(),
                        }
                    }),
                    diagnostic: Some(diagnostic),
                    decomposition: None,
                    acknowledged_directive_ids: Vec::new(),
                    applied_directive_ids: Vec::new(),
                },
                unix_time_ms(),
            )
            .await;
    }

    async fn worker_finished(&self, attempt_id: &PlanAttemptId) {
        self.inner.running.lock().await.remove(attempt_id);
        self.inner.notify.notify_one();
    }
}

fn worker_task(node: &PlanNodeSnapshot) -> Result<(SubagentTaskSpec, GenerationConfig), String> {
    let mut task = SubagentTaskSpec::new(worker_contract_text(node), u32::MAX)
        .map_err(|error| error.to_string())?
        .with_display_name(Some(node.objective.clone()))
        .with_allowed_tools(node.harness.allowed_tools.clone())
        .with_expected_output(Some(
            "Resolve the lease with report_plan_attempt; free-form final text is not completion."
                .to_owned(),
        ));
    task = task
        .with_read_scope(node.harness.read_scope.iter().map(PathBuf::from))
        .map_err(|error| error.to_string())?;
    task = task
        .with_write_scope(node.harness.write_scope.iter().map(PathBuf::from))
        .map_err(|error| error.to_string())?;
    task = task
        .with_forbidden_paths(node.harness.forbidden_paths.iter().map(PathBuf::from))
        .map_err(|error| error.to_string())?;
    let reasoning_effort = node
        .harness
        .reasoning_effort
        .as_deref()
        .map(ReasoningEffort::new)
        .transpose()
        .map_err(|error| error.to_string())?;
    task = task.with_reasoning_effort(reasoning_effort.clone());
    Ok((
        task,
        GenerationConfig::default().with_reasoning_effort(reasoning_effort),
    ))
}

fn worker_contract_text(node: &PlanNodeSnapshot) -> String {
    format!(
        "Execute only the leased plan node. Objective: {}\nAcceptance:\n{}\nUse report_plan_progress for durable semantic checkpoints. Finish exactly once with report_plan_attempt. Decompose only into direct children when the node genuinely requires it.",
        node.objective,
        node.acceptance
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn worker_initial_prompt(snapshot: &PlanSnapshot, node_id: &PlanNodeId) -> String {
    snapshot
        .nodes
        .iter()
        .find(|node| &node.id == node_id)
        .map(worker_contract_text)
        .unwrap_or_else(|| "Read the leased plan context and report a typed blocker.".to_owned())
}

fn ready_node_ids(snapshot: &PlanSnapshot) -> Vec<PlanNodeId> {
    let completed = snapshot
        .nodes
        .iter()
        .filter(|node| node.status == PlanNodeStatus::Completed)
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let live_leases = snapshot
        .leases
        .iter()
        .filter(|lease| lease.status == PlanLeaseStatus::Live)
        .map(|lease| lease.node_id.clone())
        .collect::<BTreeSet<_>>();
    let mut ready = snapshot
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.status,
                PlanNodeStatus::Pending | PlanNodeStatus::Verifying
            )
        })
        .filter(|node| !live_leases.contains(&node.id))
        .filter(|node| node.depends_on.iter().all(|id| completed.contains(id)))
        .filter(|node| node_shape_is_ready(snapshot, node))
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    ready.sort_by_key(|id| node_order_key(snapshot, id));
    ready
}

fn node_shape_is_ready(snapshot: &PlanSnapshot, node: &PlanNodeSnapshot) -> bool {
    let children = snapshot
        .nodes
        .iter()
        .filter(|candidate| {
            candidate.parent_id.as_ref() == Some(&node.id)
                && candidate.status != PlanNodeStatus::Superseded
        })
        .collect::<Vec<_>>();
    if children.is_empty() {
        return node.status == PlanNodeStatus::Pending;
    }
    node.status == PlanNodeStatus::Verifying
        && children
            .iter()
            .all(|child| child.status == PlanNodeStatus::Completed)
}

fn node_order_key(snapshot: &PlanSnapshot, node_id: &PlanNodeId) -> Vec<u16> {
    let mut order = Vec::new();
    let mut cursor = snapshot.nodes.iter().find(|node| &node.id == node_id);
    while let Some(node) = cursor {
        order.push(node.sibling_order);
        cursor = node
            .parent_id
            .as_ref()
            .and_then(|parent_id| snapshot.nodes.iter().find(|node| &node.id == parent_id));
    }
    order.reverse();
    order
}

fn write_scopes_overlap(left: &[String], right: &[String]) -> bool {
    left.iter().any(|left| {
        right
            .iter()
            .any(|right| paths_overlap(Path::new(left), Path::new(right)))
    })
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}
