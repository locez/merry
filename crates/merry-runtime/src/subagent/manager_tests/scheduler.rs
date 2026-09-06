use super::*;

#[tokio::test(flavor = "current_thread")]
async fn manager_rejects_overlapping_write_scopes_before_spawn() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        factory.clone(),
    );
    let first = SubagentTaskSpec::new("Edit one.", 2)
        .expect("valid")
        .with_write_scope(["src/lib.rs"])
        .expect("valid scope");
    let second = SubagentTaskSpec::new("Edit two.", 2)
        .expect("valid")
        .with_write_scope(["src"])
        .expect("valid scope");

    let output = manager
        .spawn(vec![first, second], Some(2), CancellationToken::new())
        .await
        .expect("spawn tool should return a structured result");

    assert!(output.spawned.is_empty());
    assert_eq!(output.rejected.len(), 2);
    assert_eq!(factory.started.load(Ordering::SeqCst), 0);
    assert_eq!(manager.snapshot().await.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn manager_starts_children_under_max_concurrency() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::new(1, 1).expect("valid config"),
        factory.clone(),
    );
    let first = SubagentTaskSpec::new("First task.", 2).expect("valid");
    let second = SubagentTaskSpec::new("Second task.", 2).expect("valid");

    let output = manager
        .spawn(vec![first, second], Some(2), CancellationToken::new())
        .await
        .expect("spawn should succeed");

    assert_eq!(output.spawned.len(), 2);
    assert_eq!(
        output.spawned[0].status,
        SpawnedSubagentStatusLabel::Running
    );
    assert_eq!(output.spawned[1].status, SpawnedSubagentStatusLabel::Queued);
    assert_eq!(factory.started.load(Ordering::SeqCst), 1);
    assert_eq!(manager.snapshot().await.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn manager_counts_existing_open_children_against_global_thread_limit() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::new(1, 1).expect("valid config"),
        factory.clone(),
    );

    manager
        .spawn(
            vec![SubagentTaskSpec::new("First open task.", 2).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("first spawn should succeed");
    let second = manager
        .spawn(
            vec![SubagentTaskSpec::new("Second queued task.", 2).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("second spawn should succeed");

    assert_eq!(second.spawned[0].status, SpawnedSubagentStatusLabel::Queued);
    assert_eq!(factory.started.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_zero_concurrency_leaves_spawned_children_queued() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        factory.clone(),
    );

    let output = manager
        .spawn(
            vec![SubagentTaskSpec::new("Queued task.", 2).expect("valid")],
            Some(0),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");

    assert_eq!(output.spawned[0].status, SpawnedSubagentStatusLabel::Queued);
    assert_eq!(factory.started.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn queued_child_is_promoted_after_running_child_completes() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::new(1, 1).expect("valid config"),
        factory.clone(),
    );

    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Complete first.", 1).expect("valid"),
                SubagentTaskSpec::new("Promote second.", 1).expect("valid"),
            ],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let second_id = output.spawned[1].agent_id.clone();

    let second = manager
        .wait(
            std::slice::from_ref(&second_id),
            WaitMode::All,
            Some(Duration::from_millis(100)),
        )
        .await
        .expect("wait should return promoted child");

    assert_eq!(factory.started.load(Ordering::SeqCst), 2);
    assert_eq!(second.agents.len(), 1);
    assert_ne!(second.agents[0].status, SubagentStatusLabel::Queued);
}

#[tokio::test(flavor = "current_thread")]
async fn queued_promotion_respects_spawn_batch_max_concurrency() {
    let factory = Arc::new(PendingChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::new(2, 1).expect("valid config"),
        factory.clone(),
    );

    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Run first until max steps.", 1).expect("valid"),
                SubagentTaskSpec::new("Promote second and keep it pending.", 2).expect("valid"),
                SubagentTaskSpec::new("Remain queued by batch cap.", 2).expect("valid"),
            ],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let third_id = output.spawned[2].agent_id.clone();

    tokio::time::timeout(Duration::from_millis(100), async {
        while factory.started.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second child should start before timeout");
    tokio::task::yield_now().await;
    let snapshot = manager.snapshot().await;

    assert_eq!(factory.started.load(Ordering::SeqCst), 2);
    assert_eq!(
        snapshot
            .iter()
            .find(|agent| agent.agent_id == third_id)
            .expect("third child remains tracked")
            .status,
        SubagentStatusLabel::Queued
    );
}

#[tokio::test(flavor = "current_thread")]
async fn zero_concurrency_batch_is_not_promoted_by_unrelated_completion() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::new(2, 1).expect("valid config"),
        factory.clone(),
    );
    let zero_batch = manager
        .spawn(
            vec![SubagentTaskSpec::new("Stay queued.", 1).expect("valid")],
            Some(0),
            CancellationToken::new(),
        )
        .await
        .expect("zero concurrency spawn should succeed");
    let queued_id = zero_batch.spawned[0].agent_id.clone();
    let running_batch = manager
        .spawn(
            vec![SubagentTaskSpec::new("Complete unrelated.", 1).expect("valid")],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("running spawn should succeed");
    let running_id = running_batch.spawned[0].agent_id.clone();

    manager
        .wait(
            std::slice::from_ref(&running_id),
            WaitMode::All,
            Some(Duration::from_millis(100)),
        )
        .await
        .expect("unrelated running child should complete");
    let snapshot = manager.snapshot().await;

    assert_eq!(
        snapshot
            .iter()
            .find(|agent| agent.agent_id == queued_id)
            .expect("zero concurrency child remains tracked")
            .status,
        SubagentStatusLabel::Queued
    );
    assert_eq!(factory.started.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn queued_child_is_promoted_after_running_child_is_cancelled() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::new(1, 1).expect("valid config"),
        factory.clone(),
    );

    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Cancel first.", 2).expect("valid"),
                SubagentTaskSpec::new("Promote after cancel.", 1).expect("valid"),
            ],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let first_id = output.spawned[0].agent_id.clone();
    let second_id = output.spawned[1].agent_id.clone();

    manager
        .cancel(std::slice::from_ref(&first_id))
        .await
        .expect("cancel should succeed");
    let second = manager
        .wait(
            std::slice::from_ref(&second_id),
            WaitMode::All,
            Some(Duration::from_millis(100)),
        )
        .await
        .expect("wait should return promoted child");

    assert_eq!(factory.started.load(Ordering::SeqCst), 2);
    assert_eq!(second.agents.len(), 1);
    assert_ne!(second.agents[0].status, SubagentStatusLabel::Queued);
}

#[tokio::test(flavor = "current_thread")]
async fn queued_child_is_promoted_after_factory_failure() {
    let factory = Arc::new(FailsFirstChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::new(1, 1).expect("valid config"),
        factory.clone(),
    );

    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Fail factory.", 1).expect("valid"),
                SubagentTaskSpec::new("Promote after failure.", 1).expect("valid"),
            ],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let second_id = output.spawned[1].agent_id.clone();

    let second = manager
        .wait(
            std::slice::from_ref(&second_id),
            WaitMode::All,
            Some(Duration::from_millis(100)),
        )
        .await
        .expect("wait should return promoted child");

    assert_eq!(factory.calls(), 2);
    assert_eq!(second.agents.len(), 1);
    assert_ne!(second.agents[0].status, SubagentStatusLabel::Queued);
}

#[tokio::test(flavor = "current_thread")]
async fn max_length_parent_session_still_starts_random_child_session() {
    let factory = Arc::new(FakeChildFactory::new());
    let manager = SubagentManager::new(
        SessionId::new(&"p".repeat(128)).expect("max length parent id is valid"),
        SubagentConfig::new(1, 1).expect("valid config"),
        factory.clone(),
    );

    let output = manager
        .spawn(
            vec![
                SubagentTaskSpec::new("Fail child id.", 1).expect("valid"),
                SubagentTaskSpec::new("Promote after child id failure.", 1).expect("valid"),
            ],
            Some(1),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should return structured statuses");

    assert_eq!(output.spawned.len(), 2);
    assert_eq!(
        output.spawned[0].status,
        SpawnedSubagentStatusLabel::Running
    );
    assert_eq!(output.spawned[1].status, SpawnedSubagentStatusLabel::Queued);
    assert_eq!(factory.started.load(Ordering::SeqCst), 1);
}
