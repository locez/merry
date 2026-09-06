use super::*;

#[tokio::test]
async fn runtime_controlled_manager_rejects_spawn_after_it_is_disabled() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::runtime_controlled(
        SessionId::new("dynamic-subagents-disabled").expect("valid session id"),
        SubagentConfig::default(),
        factory.clone(),
        true,
    );
    manager
        .update_policy(false, SubagentConfig::default())
        .await
        .expect("policy update should apply");

    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Must not start.", 1).expect("valid task")],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("disabled spawn returns a structured rejection");

    assert!(output.spawned.is_empty());
    assert_eq!(output.rejected.len(), 1);
    assert_eq!(output.rejected[0].reason, "subagent spawning is disabled");
    assert_eq!(factory.started.load(Ordering::SeqCst), 0);
    assert!(manager.snapshot().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_controlled_manager_promotes_queued_children_after_thread_limit_increase() {
    let factory = Arc::new(AlwaysPendingChildFactory::new());
    let manager = SubagentManager::runtime_controlled(
        SessionId::new("dynamic-subagents-threads").expect("valid session id"),
        SubagentConfig::new(1, 1).expect("valid initial config"),
        factory.clone(),
        true,
    );
    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("First pending task.", 1).expect("valid task"),
                SubagentTaskSpec::new("Second queued task.", 1).expect("valid task"),
            ],
            None,
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");

    assert_eq!(
        output.spawned[0].status,
        SpawnedSubagentStatusLabel::Running
    );
    assert_eq!(output.spawned[1].status, SpawnedSubagentStatusLabel::Queued);
    assert_eq!(factory.started.load(Ordering::SeqCst), 1);

    manager
        .update_policy(
            true,
            SubagentConfig::new(2, 1).expect("valid updated config"),
        )
        .await
        .expect("thread limit update should apply");

    assert_eq!(factory.started.load(Ordering::SeqCst), 2);
    let statuses = manager.snapshot().await;
    assert_eq!(
        statuses
            .iter()
            .filter(|agent| agent.status == SubagentStatusLabel::Running)
            .count(),
        2
    );
}
