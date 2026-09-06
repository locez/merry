use super::*;

#[tokio::test(flavor = "current_thread")]
async fn plan_link_scope_requires_current_exact_active_binding() {
    let session_id = SessionId::new("scope-exact-binding").expect("valid session id");
    let session = Arc::new(tokio::sync::Mutex::new(crate::session::SessionState::new(
        session_id,
    )));
    let (controller, _events) = PlanController::start(
        Arc::clone(&session),
        None,
        std::num::NonZeroUsize::new(16).expect("non-zero event buffer"),
    );
    controller
        .begin(crate::plan::BeginPlanInput {
            reason: "validate exact linked scope".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activation succeeds");
    controller
        .update(crate::plan::UpdatePlanInput {
            reason: "define linked scope task".to_owned(),
            execution_intent: crate::plan::PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: crate::plan::PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: crate::plan::PlanNodeInput {
                    id: None,
                    client_key: Some("owned".to_owned()),
                    objective: "Complete the owned task".to_owned(),
                    acceptance: vec!["owned task completes".to_owned()],
                    status: None,
                    executor_policy: merry_core::PlanExecutorPolicy::Delegate,
                    harness: merry_core::PlanHarnessSnapshot::default(),
                    recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                    depends_on: Vec::new(),
                    children: Vec::new(),
                },
            },
        })
        .await
        .expect("plan definition succeeds");
    let link = controller
        .bind_subagent(
            "owned".to_owned(),
            merry_core::SubagentId::new("scope-agent").expect("valid agent id"),
            merry_core::SubagentTaskId::new("scope-task").expect("valid task id"),
            1,
        )
        .await
        .expect("link binds");
    let runtime = plan_link_runtime_for_controller(controller.clone());

    assert!(
        runtime
            .scope_for_link(&link)
            .await
            .expect("matching scope lookup succeeds")
            .is_some()
    );

    for status in [
        merry_core::PlanLinkStatus::Completed,
        merry_core::PlanLinkStatus::Failed,
        merry_core::PlanLinkStatus::Cancelled,
        merry_core::PlanLinkStatus::Superseded,
    ] {
        let mut terminal = link.clone();
        terminal.status = status;
        assert!(
            runtime
                .scope_for_link(&terminal)
                .await
                .expect("terminal scope lookup succeeds")
                .is_none(),
            "terminal input status {status:?} must not create a scope"
        );
    }

    let mut foreign_plan = link.clone();
    foreign_plan.plan_id = merry_core::PlanId::new("foreign-plan").expect("valid plan id");
    assert!(
        runtime
            .scope_for_link(&foreign_plan)
            .await
            .expect("foreign plan scope lookup succeeds")
            .is_none()
    );

    let mut foreign_node = link.clone();
    foreign_node.node_id = merry_core::PlanNodeId::new("foreign-node").expect("valid node id");
    assert!(
        runtime
            .scope_for_link(&foreign_node)
            .await
            .expect("foreign node scope lookup succeeds")
            .is_none()
    );

    let mut foreign_binding = link.clone();
    foreign_binding.binding_id =
        merry_core::PlanBindingId::new("foreign-binding").expect("valid binding id");
    assert!(
        runtime
            .scope_for_link(&foreign_binding)
            .await
            .expect("foreign binding scope lookup succeeds")
            .is_none()
    );

    let mut foreign_subagent = link.clone();
    foreign_subagent.subagent_id =
        merry_core::SubagentId::new("foreign-agent").expect("valid agent id");
    assert!(
        runtime
            .scope_for_link(&foreign_subagent)
            .await
            .expect("foreign subagent scope lookup succeeds")
            .is_none()
    );

    let mut foreign_task = link.clone();
    foreign_task.task_id = merry_core::SubagentTaskId::new("foreign-task").expect("valid task id");
    assert!(
        runtime
            .scope_for_link(&foreign_task)
            .await
            .expect("foreign task scope lookup succeeds")
            .is_none()
    );

    for (now_ms, status) in [
        (2, merry_core::PlanLinkStatus::Completed),
        (3, merry_core::PlanLinkStatus::Failed),
        (4, merry_core::PlanLinkStatus::Cancelled),
        (5, merry_core::PlanLinkStatus::Superseded),
    ] {
        controller
            .update_subagent_link(link.binding_id.clone(), status, now_ms)
            .await
            .expect("link status transition commits");
        assert!(
            runtime
                .scope_for_link(&link)
                .await
                .expect("terminal controller lookup succeeds")
                .is_none(),
            "a stale active link must not retain scope after {status:?} controller transition"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn linked_spawn_updates_plan_from_active_to_completed() {
    let session_id = SessionId::new("subagent-plan-link").expect("valid session id");
    let session = Arc::new(tokio::sync::Mutex::new(crate::session::SessionState::new(
        session_id.clone(),
    )));
    let (controller, _events) = PlanController::start(
        Arc::clone(&session),
        None,
        std::num::NonZeroUsize::new(16).expect("non-zero event buffer"),
    );
    controller
        .begin(crate::plan::BeginPlanInput {
            reason: "link child lifecycle".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activation succeeds");
    controller
        .update(crate::plan::UpdatePlanInput {
            reason: "define linked task".to_owned(),
            execution_intent: crate::plan::PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: crate::plan::PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: crate::plan::PlanNodeInput {
                    id: None,
                    client_key: Some("root".to_owned()),
                    objective: "Complete the linked task".to_owned(),
                    acceptance: vec!["child completes".to_owned()],
                    status: None,
                    executor_policy: merry_core::PlanExecutorPolicy::Delegate,
                    harness: merry_core::PlanHarnessSnapshot::default(),
                    recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                    depends_on: Vec::new(),
                    children: Vec::new(),
                },
            },
        })
        .await
        .expect("plan definition succeeds");
    let manager = SubagentManager::new(
        session_id,
        SubagentConfig::new(1, 1).expect("valid subagent config"),
        Arc::new(RecordingModelChildFactory::new()),
    );
    manager.attach_plan_link_runtime(plan_link_runtime_for_controller(controller.clone()));
    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Complete the linked task.", 2)
                    .expect("valid task")
                    .with_plan_client_key(Some("root".to_owned())),
            ],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("linked spawn succeeds");
    assert_eq!(output.spawned.len(), 1);

    manager
        .wait(
            &[output.spawned[0].agent_id.clone()],
            WaitMode::All,
            Some(Duration::from_secs(2)),
        )
        .await
        .expect("child wait succeeds");
    let snapshot = controller
        .snapshot()
        .await
        .expect("plan snapshot reads")
        .expect("active plan exists");
    let node = snapshot
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some("root"))
        .expect("linked node exists");
    assert_eq!(node.execution_summary.active, 0);
    assert_eq!(node.execution_summary.completed, 1);
    assert_eq!(node.links.len(), 1);
    assert_eq!(node.links[0].status, merry_core::PlanLinkStatus::Completed);
}

#[tokio::test(flavor = "current_thread")]
async fn linked_children_complete_the_plan_without_follow_up_update() {
    let session_id = SessionId::new("subagent-plan-links").expect("valid session id");
    let session = Arc::new(tokio::sync::Mutex::new(crate::session::SessionState::new(
        session_id.clone(),
    )));
    let (controller, _events) = PlanController::start(
        Arc::clone(&session),
        None,
        std::num::NonZeroUsize::new(16).expect("non-zero event buffer"),
    );
    controller
        .begin(crate::plan::BeginPlanInput {
            reason: "link parallel child lifecycles".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activation succeeds");
    controller
        .update(crate::plan::UpdatePlanInput {
            reason: "define parallel linked tasks".to_owned(),
            execution_intent: crate::plan::PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: Some(2),
            change: crate::plan::PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: crate::plan::PlanNodeInput {
                    id: None,
                    client_key: Some("root".to_owned()),
                    objective: "Complete both linked tasks".to_owned(),
                    acceptance: vec!["both children complete".to_owned()],
                    status: None,
                    executor_policy: merry_core::PlanExecutorPolicy::Local,
                    harness: merry_core::PlanHarnessSnapshot::default(),
                    recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                    depends_on: Vec::new(),
                    children: vec![
                        crate::plan::PlanNodeInput {
                            id: None,
                            client_key: Some("left".to_owned()),
                            objective: "Complete the left task".to_owned(),
                            acceptance: vec!["left child completes".to_owned()],
                            status: None,
                            executor_policy: merry_core::PlanExecutorPolicy::Delegate,
                            harness: merry_core::PlanHarnessSnapshot::default(),
                            recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                            depends_on: Vec::new(),
                            children: Vec::new(),
                        },
                        crate::plan::PlanNodeInput {
                            id: None,
                            client_key: Some("right".to_owned()),
                            objective: "Complete the right task".to_owned(),
                            acceptance: vec!["right child completes".to_owned()],
                            status: None,
                            executor_policy: merry_core::PlanExecutorPolicy::Delegate,
                            harness: merry_core::PlanHarnessSnapshot::default(),
                            recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                            depends_on: Vec::new(),
                            children: Vec::new(),
                        },
                    ],
                },
            },
        })
        .await
        .expect("plan definition succeeds");

    controller
        .authorize_execution(
            merry_core::PlanCapabilityEnvelopeSnapshot::default(),
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("execution authorization succeeds");

    let manager = SubagentManager::new(
        session_id,
        SubagentConfig::new(2, 1).expect("valid subagent config"),
        Arc::new(RecordingModelChildFactory::new()),
    );
    manager.attach_plan_link_runtime(plan_link_runtime_for_controller(controller.clone()));
    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Complete the left task.", 2)
                    .expect("valid left task")
                    .with_plan_client_key(Some("left".to_owned())),
                SubagentTaskSpec::new("Complete the right task.", 2)
                    .expect("valid right task")
                    .with_plan_client_key(Some("right".to_owned())),
            ],
            Some(2),
            CancellationToken::new(),
        )
        .await
        .expect("linked spawn succeeds");
    let agent_ids = output
        .spawned
        .iter()
        .map(|agent| agent.agent_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(agent_ids.len(), 2);

    manager
        .wait(&agent_ids, WaitMode::All, Some(Duration::from_secs(2)))
        .await
        .expect("child wait succeeds");

    let snapshot = controller
        .snapshot()
        .await
        .expect("plan snapshot reads")
        .expect("active plan exists");
    let linked_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| matches!(node.client_key.as_deref(), Some("left" | "right")))
        .collect::<Vec<_>>();
    assert_eq!(linked_nodes.len(), 2);
    assert!(linked_nodes.iter().all(|node| {
        node.execution_summary.active == 0
            && node.execution_summary.completed == 1
            && node.links.len() == 1
            && node.links[0].status == merry_core::PlanLinkStatus::Completed
    }));
    assert_ne!(
        linked_nodes[0].links[0].binding_id,
        linked_nodes[1].links[0].binding_id
    );
    assert_eq!(snapshot.phase, merry_core::PlanPhase::Completed);
}

#[tokio::test(flavor = "current_thread")]
async fn nested_subagent_keeps_parent_plan_link_runtime() {
    let session_id = SessionId::new("nested-subagent-plan-link").expect("valid session id");
    let session = Arc::new(tokio::sync::Mutex::new(crate::session::SessionState::new(
        session_id.clone(),
    )));
    let (controller, _events) = PlanController::start(
        Arc::clone(&session),
        None,
        std::num::NonZeroUsize::new(16).expect("non-zero event buffer"),
    );
    controller
        .begin(crate::plan::BeginPlanInput {
            reason: "link nested children".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activation succeeds");
    controller
        .update(crate::plan::UpdatePlanInput {
            reason: "define nested linked task".to_owned(),
            execution_intent: crate::plan::PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: crate::plan::PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: crate::plan::PlanNodeInput {
                    id: None,
                    client_key: Some("root".to_owned()),
                    objective: "Complete nested linked work".to_owned(),
                    acceptance: vec!["nested child completes".to_owned()],
                    status: None,
                    executor_policy: merry_core::PlanExecutorPolicy::Delegate,
                    harness: merry_core::PlanHarnessSnapshot::default(),
                    recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                    depends_on: Vec::new(),
                    children: Vec::new(),
                },
            },
        })
        .await
        .expect("plan definition succeeds");

    #[derive(Clone)]
    struct CapturingFactory {
        input: Arc<StdMutex<Option<ChildRuntimeInput>>>,
    }

    impl ChildRuntimeFactory for CapturingFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
            *self
                .input
                .lock()
                .expect("child input mutex is not poisoned") = Some(input.clone());
            let mut builder = Runtime::builder(input.session_id).task_anchor(input.task_anchor);
            if let Some(hub) = input.activity_hub {
                builder = builder.subagent_activity_hub(hub);
            }
            builder.build()
        }
    }

    let captured = Arc::new(StdMutex::new(None));
    let activity_hub = Arc::new(SubagentActivityHub::new());
    let root_manager = SubagentManager::runtime_controlled(
        session_id.clone(),
        SubagentConfig::new(1, 2).expect("valid root config"),
        Arc::new(CapturingFactory {
            input: Arc::clone(&captured),
        }),
        true,
    );
    root_manager.attach_activity_hub(Arc::clone(&activity_hub));
    root_manager.attach_plan_link_runtime(plan_link_runtime_for_controller(controller.clone()));
    let root = root_manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Complete root-linked work.", 1)
                    .expect("valid root task")
                    .with_plan_client_key(Some("root".to_owned())),
            ],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("root spawn succeeds");
    assert_eq!(root.spawned.len(), 1);

    let nested_link_runtime = captured
        .lock()
        .expect("child input mutex is not poisoned")
        .as_ref()
        .and_then(|input| input.plan_link_runtime.clone())
        .expect("child receives the parent Plan link runtime");
    let forwarded_activity_hub = captured
        .lock()
        .expect("child input mutex is not poisoned")
        .as_ref()
        .and_then(|input| input.activity_hub.clone())
        .expect("child receives the parent activity hub");
    assert!(Arc::ptr_eq(&forwarded_activity_hub, &activity_hub));
    assert!(
        captured
            .lock()
            .expect("child input mutex is not poisoned")
            .as_ref()
            .and_then(|input| input.plan_subagent_scope.as_ref())
            .is_some(),
        "linked child receives the opaque Plan subtree scope"
    );
    let nested_captured = Arc::new(StdMutex::new(None));
    let nested_manager = SubagentManager::runtime_controlled_at_depth(
        SessionId::new("nested-parent").expect("valid nested session id"),
        SubagentConfig::new(1, 2).expect("valid nested config"),
        Arc::new(CapturingFactory {
            input: Arc::clone(&nested_captured),
        }),
        true,
        1,
    );
    nested_manager.attach_activity_hub(Arc::clone(&forwarded_activity_hub));
    nested_manager.attach_plan_link_runtime(nested_link_runtime);
    let nested = nested_manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Complete nested linked work.", 1)
                    .expect("valid nested task")
                    .with_plan_client_key(Some("root".to_owned())),
            ],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("nested spawn succeeds");
    assert_eq!(nested.spawned.len(), 1);
    let nested_activity_hub = nested_captured
        .lock()
        .expect("nested child input mutex is not poisoned")
        .as_ref()
        .and_then(|input| input.activity_hub.clone())
        .expect("nested child receives the shared activity hub");
    assert!(Arc::ptr_eq(&nested_activity_hub, &activity_hub));

    let snapshot = controller
        .snapshot()
        .await
        .expect("plan snapshot reads")
        .expect("active plan exists");
    let node = snapshot
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some("root"))
        .expect("linked node exists");
    assert_eq!(node.links.len(), 2);
}
