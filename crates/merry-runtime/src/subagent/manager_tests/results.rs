use super::*;

#[tokio::test(flavor = "current_thread")]
async fn manager_wait_returns_compact_statuses() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        factory,
    );
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Complete task.", 1).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let agent_id = output.spawned[0].agent_id.clone();

    let wait = manager
        .wait(&[agent_id], WaitMode::All, Some(Duration::from_millis(10)))
        .await
        .expect("wait should return status");

    assert_eq!(wait.agents.len(), 1);
    assert!(matches!(
        wait.agents[0].status,
        SubagentStatusLabel::Completed | SubagentStatusLabel::Failed | SubagentStatusLabel::Running
    ));
    assert!(wait.agents[0].summary.len() < 256);
    assert!(wait.agents[0].output_paths.len() <= 1);
}

#[tokio::test(flavor = "current_thread")]
async fn completed_child_has_no_output_paths_until_artifact_handoff_exists() {
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        Arc::new(FakeChildFactory::new()),
    );
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Complete without artifact.", 1).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let agent_id = output.spawned[0].agent_id.clone();

    let wait = manager
        .wait(
            std::slice::from_ref(&agent_id),
            WaitMode::All,
            Some(Duration::from_millis(100)),
        )
        .await
        .expect("wait should return terminal child");

    assert_eq!(wait.agents[0].status, SubagentStatusLabel::Completed);
    assert!(wait.agents[0].output_paths.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn completed_child_reports_explicit_result_and_changed_paths() {
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        Arc::new(ReportingChildFactory),
    );
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Patch the status file.", 4).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let agent_id = output.spawned[0].agent_id.clone();

    let wait = manager
        .wait(
            std::slice::from_ref(&agent_id),
            WaitMode::All,
            Some(Duration::from_millis(100)),
        )
        .await
        .expect("wait should return terminal child");

    assert_eq!(wait.agents[0].status, SubagentStatusLabel::Completed);
    assert_eq!(wait.agents[0].summary, "child completed");
    assert_eq!(
        wait.agents[0]
            .result
            .as_ref()
            .map(|result| result.conclusion.as_str()),
        Some("Patched subagent-output.txt to status: done.")
    );
    assert_eq!(
        wait.agents[0].changed_paths,
        vec!["subagent-output.txt".to_owned()]
    );
    assert!(wait.agents[0].output_paths.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn child_model_request_uses_task_reasoning_effort() {
    let factory = Arc::new(RecordingModelChildFactory::new());
    let activity_hub = Arc::new(SubagentActivityHub::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        factory.clone(),
    );
    manager.attach_activity_hub(activity_hub);
    let task = SubagentTaskSpec::new("Run a cheap child task.", 1)
        .expect("valid task")
        .with_reasoning_effort(Some(
            ReasoningEffort::new("low").expect("valid reasoning effort"),
        ));
    let output = manager
        .spawn(vec![task], Some(1), CancellationToken::new())
        .await
        .expect("spawn should succeed");
    let agent_id = output.spawned[0].agent_id.clone();

    manager
        .wait(
            std::slice::from_ref(&agent_id),
            WaitMode::All,
            Some(Duration::from_millis(100)),
        )
        .await
        .expect("child should complete");
    let requests = factory.recorded_requests();

    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .generation()
            .reasoning_effort()
            .map(|effort| effort.as_str()),
        Some("low")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn child_model_request_omits_reasoning_effort_when_task_does_not_set_it() {
    let factory = Arc::new(RecordingModelChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        factory.clone(),
    );
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Run a default child task.", 1).expect("valid task")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let agent_id = output.spawned[0].agent_id.clone();

    manager
        .wait(
            std::slice::from_ref(&agent_id),
            WaitMode::All,
            Some(Duration::from_millis(100)),
        )
        .await
        .expect("child should complete");
    let requests = factory.recorded_requests();

    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].generation().reasoning_effort(), None);
}

#[tokio::test(flavor = "current_thread")]
async fn manager_cancel_marks_selected_children_cancelled() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::new(1, 1).expect("valid config"),
        factory,
    );
    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Cancel this task.", 2).expect("valid"),
                SubagentTaskSpec::new("Leave this task queued.", 2).expect("valid"),
            ],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");

    let cancelled_id = output.spawned[0].agent_id.clone();
    let untouched_id = output.spawned[1].agent_id.clone();
    let cancelled = manager
        .cancel(std::slice::from_ref(&cancelled_id))
        .await
        .expect("cancel should return selected status");
    let snapshot = manager.snapshot().await;

    assert_eq!(cancelled.agents.len(), 1);
    assert_eq!(cancelled.agents[0].status, SubagentStatusLabel::Cancelled);
    assert_eq!(
        snapshot
            .iter()
            .find(|agent| agent.agent_id == cancelled_id)
            .expect("cancelled child remains in snapshot")
            .status,
        SubagentStatusLabel::Cancelled
    );
    assert_ne!(
        snapshot
            .iter()
            .find(|agent| agent.agent_id == untouched_id)
            .expect("untouched child remains in snapshot")
            .status,
        SubagentStatusLabel::Cancelled
    );
}

#[tokio::test(flavor = "current_thread")]
async fn manager_cancel_cancels_selected_child_token() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        factory,
    );
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Cancel token.", 2).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let agent_id = output.spawned[0].agent_id.clone();
    let token = manager
        .cancellation_token_for_test(&agent_id)
        .await
        .expect("managed token exists");

    manager
        .cancel(&[agent_id])
        .await
        .expect("cancel should succeed");

    assert!(token.is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn manager_cancel_does_not_rewrite_terminal_children() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        factory,
    );
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Complete then cancel.", 1).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let agent_id = output.spawned[0].agent_id.clone();

    manager
        .wait(
            std::slice::from_ref(&agent_id),
            WaitMode::All,
            Some(Duration::from_millis(100)),
        )
        .await
        .expect("wait should complete");
    let cancelled = manager
        .cancel(std::slice::from_ref(&agent_id))
        .await
        .expect("cancel should return selected terminal status");

    assert_eq!(cancelled.agents.len(), 1);
    assert_ne!(cancelled.agents[0].status, SubagentStatusLabel::Cancelled);
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_child_completion_does_not_overwrite_parent_cancellation() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        factory,
    );
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Cancel before loop settles.", 2).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let agent_id = output.spawned[0].agent_id.clone();

    manager
        .cancel(std::slice::from_ref(&agent_id))
        .await
        .expect("cancel should succeed");
    tokio::task::yield_now().await;
    let snapshot = manager.snapshot().await;

    assert_eq!(
        snapshot
            .iter()
            .find(|agent| agent.agent_id == agent_id)
            .expect("child remains in snapshot")
            .status,
        SubagentStatusLabel::Cancelled
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_child_start_failure_handler_does_not_overwrite_parent_cancellation() {
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        Arc::new(FakeChildFactory::new()),
    );
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Cancel before start failure handler.", 2).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let agent_id = output.spawned[0].agent_id.clone();

    manager
        .cancel(std::slice::from_ref(&agent_id))
        .await
        .expect("cancel should succeed");
    manager
        .mark_failed_and_schedule(
            &agent_id,
            "child runtime start failed",
            error_info(
                "subagent_start_error",
                "synthetic failure after cancellation",
            ),
        )
        .await;
    let snapshot = manager.snapshot().await;

    assert_eq!(
        snapshot
            .iter()
            .find(|agent| agent.agent_id == agent_id)
            .expect("child remains in snapshot")
            .status,
        SubagentStatusLabel::Cancelled
    );
}

#[tokio::test(flavor = "current_thread")]
async fn startup_error_after_terminal_claim_does_not_republish_terminal_activity() {
    let hub = Arc::new(SubagentActivityHub::new());
    let manager = SubagentManager::new(
        SessionId::new("startup-error-terminal-race").expect("valid session id"),
        SubagentConfig::default(),
        Arc::new(FakeChildFactory::new()),
    );
    manager.attach_activity_hub(Arc::clone(&hub));

    let task = SubagentTaskSpec::new("Fail after parent cancellation.", 1).expect("valid task");
    let task_anchor = TaskAnchor::new(task.task()).expect("valid task anchor");
    let agent_id = SubagentId::new("agent-terminal-race").expect("valid agent id");
    let task_id = SubagentTaskId::new("task-terminal-race").expect("valid task id");
    let cancellation_token = CancellationToken::new();
    {
        let mut state = manager.state.lock().await;
        state
            .batches
            .insert(1, SubagentBatch { max_concurrency: 1 });
        state.agents.insert(
            agent_id.clone(),
            ManagedSubagent {
                batch_id: 1,
                agent_id: agent_id.clone(),
                task_id: task_id.clone(),
                task: task.clone(),
                task_anchor: task_anchor.clone(),
                status: SubagentStatusLabel::Running,
                summary: "child running".to_owned(),
                result: None,
                output_paths: Vec::new(),
                changed_paths: Vec::new(),
                diagnostics: None,
                cancellation_token: cancellation_token.clone(),
                plan_link: None,
                completion_notification_acknowledged: false,
            },
        );
    }

    manager
        .cancel(std::slice::from_ref(&agent_id))
        .await
        .expect("parent cancellation should succeed");
    assert_eq!(
        hub.published_phases(),
        vec![merry_core::SubagentActivityPhase::Cancelled]
    );

    let runtime = Runtime::builder(
        SessionId::new("startup-error-terminal-child").expect("valid child session id"),
    )
    .task_anchor(task_anchor.clone())
    .build()
    .expect("child runtime should build");
    let generation_config = generation_config_for_child_task(&task);
    let launch = ChildLoopLaunch {
        agent_id: agent_id.clone(),
        task_id: task_id.clone(),
        task,
        token: cancellation_token,
        runtime,
        generation_config,
        activity_hub: Some(Arc::clone(&hub)),
    };
    let mut reducer = SubagentActivityReducer::new(agent_id.clone(), task_id);

    finish_child_with_status(
        manager.child_scheduler(),
        &launch,
        &mut reducer,
        SubagentStatusLabel::Failed,
        "child startup failed",
        error_info("subagent_start_error", "synthetic startup failure"),
    )
    .await;

    assert_eq!(
        hub.published_phases(),
        vec![merry_core::SubagentActivityPhase::Cancelled]
    );
    assert_eq!(
        manager
            .snapshot()
            .await
            .into_iter()
            .find(|agent| agent.agent_id == agent_id)
            .expect("terminal child remains tracked")
            .status,
        SubagentStatusLabel::Cancelled
    );
}

#[tokio::test(flavor = "current_thread")]
async fn manager_wait_without_timeout_observes_status_changed_before_await() {
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        Arc::new(FakeChildFactory::new()),
    );
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Already complete before wait.", 1).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let agent_id = output.spawned[0].agent_id.clone();

    loop {
        let snapshot = manager.snapshot().await;
        if snapshot
            .iter()
            .any(|agent| agent.agent_id == agent_id && agent.status.is_terminal())
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let wait = tokio::time::timeout(
        Duration::from_millis(100),
        manager.wait(std::slice::from_ref(&agent_id), WaitMode::All, None),
    )
    .await
    .expect("wait should not hang after prior completion")
    .expect("wait should succeed");

    assert_eq!(wait.agents.len(), 1);
    assert!(wait.agents[0].status.is_terminal());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_builder_stores_manager_and_runtime_returns_subagent_snapshot() {
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        Arc::new(FakeChildFactory::new()),
    );
    manager
        .spawn(
            vec![SubagentTaskSpec::new("Snapshot task.", 1).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");

    let runtime = Runtime::builder(SessionId::new("parent").expect("valid id"))
        .subagent_manager(manager)
        .build()
        .expect("runtime builds");
    let snapshot = runtime
        .subagent_snapshot()
        .await
        .expect("manager snapshot is present");

    assert_eq!(snapshot.len(), 1);
    assert!(matches!(
        snapshot[0].status,
        SubagentStatusLabel::Running | SubagentStatusLabel::Completed
    ));
}

#[test]
fn max_model_turns_result_is_exposed_as_recoverable_blocked_child() {
    let task = SubagentTaskSpec::new("Continue the implementation.", 40).expect("valid task");
    let task_anchor = TaskAnchor::new(task.task()).expect("valid task anchor");
    let agent_id = SubagentId::new("agent-budget-blocked").expect("valid agent id");
    let task_id = SubagentTaskId::new("task-budget-blocked").expect("valid task id");
    let mut agent = ManagedSubagent {
        batch_id: 1,
        agent_id,
        task_id,
        task,
        task_anchor,
        status: SubagentStatusLabel::Running,
        summary: "child running".to_owned(),
        result: None,
        output_paths: Vec::new(),
        changed_paths: Vec::new(),
        diagnostics: None,
        cancellation_token: CancellationToken::new(),
        plan_link: None,
        completion_notification_acknowledged: false,
    };
    let result = AgentLoopResult::new(
        AgentLoopStatus::Blocked {
            reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                max_model_turns: 40,
            },
        },
        Vec::new(),
        40,
        None,
        None,
    );

    apply_loop_result(&mut agent, &result, ChildLoopProjection::default());

    assert_eq!(agent.status, SubagentStatusLabel::Blocked);
    assert_eq!(
        agent.diagnostics.as_ref().map(ErrorInfo::code),
        Some("subagent_max_model_turns_reached")
    );
    let diagnostic = agent.diagnostics.as_ref().expect("blocked diagnostic");
    assert!(diagnostic.message().contains("same plan_client_key"));
    assert!(diagnostic.message().contains("larger budget"));
}
