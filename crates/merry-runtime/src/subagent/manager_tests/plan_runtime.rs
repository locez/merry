use super::*;

#[tokio::test(flavor = "current_thread")]
async fn plan_link_runtime_default_scope_lookup_is_unbound() {
    let link = PlanLinkSnapshot {
        plan_id: merry_core::PlanId::new("default-plan").expect("valid plan id"),
        node_id: merry_core::PlanNodeId::new("default-node").expect("valid node id"),
        binding_id: PlanBindingId::new("default-binding").expect("valid binding id"),
        subagent_id: SubagentId::new("default-agent").expect("valid agent id"),
        task_id: SubagentTaskId::new("default-task").expect("valid task id"),
        status: PlanLinkStatus::Active,
        linked_at_ms: 1,
        terminal_at_ms: None,
        superseded_by: None,
    };
    assert!(
        DefaultScopePlanLinkRuntime
            .scope_for_link(&link)
            .await
            .expect("default scope lookup succeeds")
            .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn child_activity_terminal_publishes_after_plan_link_update() {
    let hub = Arc::new(SubagentActivityHub::new());
    let model_release = CancellationToken::new();
    let terminal_seen_during_update = Arc::new(StdMutex::new(Vec::new()));
    let phases_during_update = Arc::new(StdMutex::new(Vec::new()));
    let update_started = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let update_completed = Arc::new(AtomicBool::new(false));
    let update_completed_notify = Arc::new(Notify::new());
    let manager = SubagentManager::new(
        SessionId::new("subagent-activity-ordering").expect("valid session id"),
        SubagentConfig::new(1, 1).expect("valid subagent config"),
        Arc::new(GatedRecordingModelChildFactory {
            release: model_release.clone(),
        }),
    );
    manager.attach_activity_hub(Arc::clone(&hub));
    manager.attach_plan_link_runtime(Arc::new(OrderingPlanLinkRuntime {
        hub: Arc::clone(&hub),
        terminal_seen_during_update: Arc::clone(&terminal_seen_during_update),
        phases_during_update: Arc::clone(&phases_during_update),
        update_started: Arc::clone(&update_started),
        release: release.clone(),
        update_completed: Arc::clone(&update_completed),
        update_completed_notify: Arc::clone(&update_completed_notify),
    }));
    let mut activity_receiver = hub.subscribe();

    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Complete the ordered child task.", 1)
                    .expect("valid task")
                    .with_plan_client_key(Some("synthetic".to_owned())),
            ],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("linked spawn succeeds");
    let agent_id = output.spawned[0].agent_id.clone();

    model_release.cancel();
    update_started.notified().await;
    let during_link_update = hub
        .current()
        .into_iter()
        .find(|snapshot| snapshot.subagent_id == agent_id)
        .expect("structured activity should precede link update");
    assert_eq!(
        during_link_update.phase,
        merry_core::SubagentActivityPhase::Running
    );
    assert!(!update_completed.load(Ordering::SeqCst));
    assert_eq!(
        phases_during_update
            .lock()
            .expect("ordering phases mutex is not poisoned")
            .as_slice(),
        &[Some(merry_core::SubagentActivityPhase::Running)]
    );

    release.cancel();
    update_completed_notify.notified().await;

    let wait = manager
        .wait(
            std::slice::from_ref(&agent_id),
            WaitMode::All,
            Some(Duration::from_secs(2)),
        )
        .await
        .expect("child wait succeeds");
    assert!(wait.terminal);
    assert_eq!(wait.agents[0].status, SubagentStatusLabel::Completed);
    assert_eq!(wait.agents[0].summary, "child completed");

    assert_eq!(
        terminal_seen_during_update
            .lock()
            .expect("ordering observations mutex is not poisoned")
            .as_slice(),
        &[false]
    );
    assert!(update_completed.load(Ordering::SeqCst));
    let activity = hub
        .current()
        .into_iter()
        .find(|snapshot| snapshot.subagent_id == agent_id)
        .expect("completed child activity is published");
    assert_eq!(activity.phase, merry_core::SubagentActivityPhase::Completed);
    assert_eq!(activity.task_id, output.spawned[0].task_id);
    assert_eq!(
        hub.published_phases(),
        vec![
            merry_core::SubagentActivityPhase::Starting,
            merry_core::SubagentActivityPhase::Running,
            merry_core::SubagentActivityPhase::Completed,
        ]
    );

    loop {
        activity_receiver
            .changed()
            .await
            .expect("terminal activity should be published");
        let snapshot = activity_receiver.borrow_and_update()[0].clone();
        if matches!(
            snapshot.phase,
            merry_core::SubagentActivityPhase::Completed
                | merry_core::SubagentActivityPhase::Failed
                | merry_core::SubagentActivityPhase::Cancelled
        ) {
            assert_eq!(snapshot.phase, merry_core::SubagentActivityPhase::Completed);
            break;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn child_bridge_request_fails_without_claiming_completed() {
    let hub = Arc::new(SubagentActivityHub::new());
    let manager = SubagentManager::new(
        SessionId::new("subagent-bridge-driver").expect("valid session id"),
        SubagentConfig::new(1, 1).expect("valid subagent config"),
        Arc::new(BridgeRequestChildFactory),
    );
    manager.attach_activity_hub(Arc::clone(&hub));

    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Use the child bridge.", 2)
                    .expect("valid task")
                    .with_allowed_tools([
                        ToolName::new("child_bridge").expect("valid bridge tool name")
                    ]),
            ],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("bridge child spawn succeeds");
    let agent_id = output.spawned[0].agent_id.clone();

    let wait = tokio::time::timeout(
        Duration::from_secs(1),
        manager.wait(std::slice::from_ref(&agent_id), WaitMode::All, None),
    )
    .await
    .expect("bridge request must not strand the child")
    .expect("bridge child wait succeeds");

    assert!(wait.terminal);
    assert_eq!(wait.agents[0].status, SubagentStatusLabel::Failed);
    assert!(wait.agents[0].result.is_none());
    assert!(
        wait.agents[0].summary.contains("bridge"),
        "unexpected bridge child summary: {}",
        wait.agents[0].summary
    );
    assert_eq!(
        hub.current()
            .into_iter()
            .find(|snapshot| snapshot.subagent_id == agent_id)
            .expect("bridge failure activity exists")
            .phase,
        merry_core::SubagentActivityPhase::Failed
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unbound_child_activity_reaches_completed_without_a_plan_link() {
    let hub = Arc::new(SubagentActivityHub::new());
    let manager = SubagentManager::new(
        SessionId::new("subagent-activity-unbound").expect("valid session id"),
        SubagentConfig::new(1, 1).expect("valid subagent config"),
        Arc::new(RecordingModelChildFactory::new()),
    );
    manager.attach_activity_hub(Arc::clone(&hub));

    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Complete an unbound child.", 1).expect("valid task")],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("unbound spawn succeeds");
    let agent_id = output.spawned[0].agent_id.clone();
    let wait = manager
        .wait(std::slice::from_ref(&agent_id), WaitMode::All, None)
        .await
        .expect("child wait succeeds");
    assert!(wait.terminal);
    assert_eq!(wait.agents[0].status, SubagentStatusLabel::Completed);
    assert_eq!(
        hub.current()
            .into_iter()
            .find(|snapshot| snapshot.subagent_id == agent_id)
            .expect("unbound child activity exists")
            .phase,
        merry_core::SubagentActivityPhase::Completed
    );
}

#[tokio::test(flavor = "current_thread")]
async fn child_completion_enqueues_a_parent_runtime_notification() {
    let manager = SubagentManager::new(
        SessionId::new("completion-notification").expect("valid session id"),
        SubagentConfig::default(),
        Arc::new(RecordingModelChildFactory::new()),
    );
    let notified = manager.completion_notify.notified();
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Complete and notify the parent.", 2).expect("valid task")],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("child spawn succeeds");
    let agent_id = output.spawned[0].agent_id.clone();

    tokio::time::timeout(Duration::from_secs(1), notified)
        .await
        .expect("child completion should wake the parent notification");
    let notifications = manager.take_completion_notifications().await;
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].agent_id, agent_id);
    assert_eq!(notifications[0].status, SubagentStatusLabel::Completed);

    let wait = manager
        .wait(std::slice::from_ref(&agent_id), WaitMode::All, None)
        .await
        .expect("child wait succeeds");
    assert!(wait.terminal);
}

#[tokio::test(flavor = "current_thread")]
async fn waiting_before_late_completion_enqueue_suppresses_notification() {
    let manager = SubagentManager::new(
        SessionId::new("completion-notification-race").expect("valid session id"),
        SubagentConfig::default(),
        Arc::new(FakeChildFactory::new()),
    );
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Complete after waiting.", 1).expect("valid task")],
            Some(0),
            CancellationToken::new(),
        )
        .await
        .expect("queued child spawn succeeds");
    let agent_id = output.spawned[0].agent_id.clone();

    {
        let mut state = manager.state.lock().await;
        let agent = state
            .agents
            .get_mut(&agent_id)
            .expect("spawned child is tracked");
        agent.status = SubagentStatusLabel::Completed;
        agent.summary = "child completed before notification delivery".to_owned();
    }

    let wait = manager
        .wait(std::slice::from_ref(&agent_id), WaitMode::All, None)
        .await
        .expect("terminal child wait succeeds");
    assert!(wait.terminal);

    manager
        .child_scheduler()
        .enqueue_completion_notification(wait.agents[0].clone())
        .await;
    assert!(manager.take_completion_notifications().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn missing_stream_result_follows_cancellation_without_claiming_completed() {
    let hub = Arc::new(SubagentActivityHub::new());
    let manager = SubagentManager::new(
        SessionId::new("subagent-activity-missing-result").expect("valid session id"),
        SubagentConfig::new(1, 1).expect("valid subagent config"),
        Arc::new(AlwaysPendingChildFactory::new()),
    );
    manager.attach_activity_hub(Arc::clone(&hub));
    let parent_token = CancellationToken::new();
    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Cancel before a stream result.", 1).expect("valid task")],
            None,
            parent_token.clone(),
        )
        .await
        .expect("pending child spawn succeeds");
    parent_token.cancel();

    let wait = manager
        .wait(
            std::slice::from_ref(&output.spawned[0].agent_id),
            WaitMode::All,
            None,
        )
        .await
        .expect("cancelled child wait succeeds");
    assert!(wait.terminal);
    assert_eq!(wait.agents[0].status, SubagentStatusLabel::Cancelled);
    assert_ne!(
        hub.current()
            .into_iter()
            .find(|snapshot| snapshot.subagent_id == output.spawned[0].agent_id)
            .expect("cancelled activity exists")
            .phase,
        merry_core::SubagentActivityPhase::Completed
    );
}

#[tokio::test(flavor = "current_thread")]
async fn plan_scope_lookup_error_fails_child_and_plan_link() {
    let factory = Arc::new(FakeChildFactory::new());
    let activity_hub = Arc::new(SubagentActivityHub::new());
    let updates = Arc::new(StdMutex::new(Vec::new()));
    let manager = SubagentManager::new(
        SessionId::new("scope-lookup-error").expect("valid session id"),
        SubagentConfig::new(1, 1).expect("valid config"),
        factory.clone(),
    );
    manager.attach_activity_hub(Arc::clone(&activity_hub));
    manager.attach_plan_link_runtime(Arc::new(FailingScopePlanLinkRuntime {
        updates: Arc::clone(&updates),
    }));

    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Fail during scope lookup.", 1)
                    .expect("valid task")
                    .with_plan_client_key(Some("synthetic".to_owned())),
            ],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn returns structured output");
    let agent_id = output.spawned[0].agent_id.clone();
    let snapshot = manager.snapshot().await;
    let agent = snapshot
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .expect("failed child remains tracked");
    assert_eq!(agent.status, SubagentStatusLabel::Failed);
    assert_eq!(
        activity_hub
            .current()
            .into_iter()
            .find(|snapshot| snapshot.subagent_id == agent_id)
            .expect("failed child activity exists")
            .phase,
        merry_core::SubagentActivityPhase::Failed
    );
    assert_eq!(factory.started.load(Ordering::SeqCst), 0);
    assert!(
        updates
            .lock()
            .expect("link updates mutex is not poisoned")
            .contains(&PlanLinkStatus::Failed)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn queued_child_cancellation_during_scope_lookup_skips_factory() {
    let factory = Arc::new(FailsFirstChildFactory::new());
    let runtime = Arc::new(BlockingScopePlanLinkRuntime {
        scope_calls: Arc::new(AtomicUsize::new(0)),
        lookup_started: Arc::new(Notify::new()),
        lookup_dropped: Arc::new(Notify::new()),
        release: CancellationToken::new(),
        updates: Arc::new(StdMutex::new(Vec::new())),
    });
    let manager = SubagentManager::new(
        SessionId::new("queued-scope-cancel").expect("valid session id"),
        SubagentConfig::new(1, 1).expect("valid config"),
        factory.clone(),
    );
    manager.attach_plan_link_runtime(runtime.clone());

    let spawn_manager = manager.clone();
    let spawn_handle = tokio::spawn(async move {
        spawn_manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Fail first start.", 1)
                        .expect("valid task")
                        .with_plan_client_key(Some("synthetic".to_owned())),
                    SubagentTaskSpec::new("Cancel during lookup.", 1)
                        .expect("valid task")
                        .with_plan_client_key(Some("synthetic".to_owned())),
                ],
                Some(1),
                CancellationToken::new(),
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), runtime.lookup_started.notified())
        .await
        .expect("queued child should enter scope lookup");
    let snapshot = manager.snapshot().await;
    let queued_agent = snapshot
        .iter()
        .find(|agent| agent.status == SubagentStatusLabel::Running)
        .expect("queued child remains reserved");
    let queued_id = queued_agent.agent_id.clone();

    manager
        .cancel(std::slice::from_ref(&queued_id))
        .await
        .expect("cancel should succeed");
    tokio::time::timeout(Duration::from_secs(1), runtime.lookup_dropped.notified())
        .await
        .expect("cancelled lookup should be dropped");
    let _output = spawn_handle
        .await
        .expect("spawn task should join")
        .expect("spawn succeeds");

    let snapshot = manager.snapshot().await;
    let cancelled = snapshot
        .iter()
        .find(|agent| agent.agent_id == queued_id)
        .expect("cancelled child remains tracked");
    assert_eq!(cancelled.status, SubagentStatusLabel::Cancelled);
    assert_eq!(factory.calls(), 1);
    assert!(
        runtime
            .updates
            .lock()
            .expect("link updates mutex is not poisoned")
            .contains(&PlanLinkStatus::Cancelled)
    );
}
