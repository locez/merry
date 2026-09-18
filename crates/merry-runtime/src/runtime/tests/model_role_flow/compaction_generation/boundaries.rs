use super::*;
use crate::compaction::compaction_request_required_tokens;

#[tokio::test(flavor = "current_thread")]
async fn compaction_reserves_numeric_repair_room_when_the_first_output_fills_the_window() {
    let oversized = VALID_CANDIDATE.replace("Old history was compacted.", &"x".repeat(6_000));
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(&oversized),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime = runtime_with_compactor("compaction-tight-repair", compactor.clone(), 64_000);
    collect_step(&runtime, &"x".repeat(200_000), StepContext::default()).await;
    collect_step(&runtime, "retained tail", StepContext::default()).await;

    runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect("the first attempt leaves room for repair")
        .expect("checkpoint installed");

    let requests = compactor.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].input().starts_with(requests[0].input()));
    assert_eq!(requests[0].generation(), requests[1].generation());
    assert_eq!(
        requests[0].tool_profile_hash(),
        requests[1].tool_profile_hash()
    );
    assert!(
        repair::repair_payload(&requests[1])
            .get("rejected_candidate")
            .is_none()
    );
    for request in &requests {
        let (input, output) = compaction_request_required_tokens(request);
        assert!(input + output < 64_000);
    }
    assert!(
        requests[0]
            .generation()
            .max_output_tokens()
            .expect("output limit")
            < 14_080
    );
}

#[tokio::test(flavor = "current_thread")]
async fn truncated_compaction_does_not_retry_at_the_declared_output_cap() {
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        vec![
            repeated_failure("non_stop"),
            completed_candidate(VALID_CANDIDATE),
        ],
        ModelCapabilities::new(true, true, false, true, Some(64_000), Some(1_024))
            .expect("capabilities"),
    );
    let runtime = runtime_with_compactor("compaction-truncated-at-cap", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;

    let result = runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await;
    assert!(matches!(
        result,
        Err(RuntimeError::CompactionModelTruncated { .. })
    ));
    assert_eq!(compactor.recorded_requests().len(), 1);
    assert!(runtime.compacted_checkpoint_summary().await.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_a_later_compaction_pass_preserves_the_checkpoint_and_releases_the_permit() {
    let (started_sender, started_receiver) = oneshot::channel();
    let (dropped_sender, dropped_receiver) = oneshot::channel();
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(VALID_CANDIDATE),
        ScriptedModelProviderResponse::PendingSetupWithDrop {
            started: started_sender,
            dropped: dropped_sender,
        },
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime = runtime_with_compactor_and_steps(
        "compaction-cancel-later-pass",
        compactor.clone(),
        32_000,
        6,
    );
    seed_rolling_history(&runtime).await;
    let policy = CitationCompactionPolicy::new(Some(10_000), Some(99_999), 1).expect("policy");
    let token = CancellationToken::new();
    let operation = runtime.compact_context_once(policy, StepContext::new(token.clone()));
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => panic!("second pass did not start: {result:?}"),
        result = started_receiver => result.expect("second pass started"),
    }
    let installed = runtime
        .compacted_checkpoint_summary()
        .await
        .expect("first pass installed");
    token.cancel();
    tokio::time::timeout(Duration::from_secs(1), &mut operation)
        .await
        .expect("cancellation returns promptly")
        .expect_err("cancelled pass fails");
    dropped_receiver.await.expect("cancelled setup released");
    assert_eq!(
        runtime.compacted_checkpoint_summary().await,
        Some(installed)
    );
    assert_eq!(compactor.recorded_requests().len(), 2);
    runtime
        .compact_context_once(policy, StepContext::default())
        .await
        .expect("the active permit was released and compaction can resume")
        .expect("remaining history compacted");
    assert_eq!(compactor.recorded_requests().len(), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn later_compaction_failure_does_not_discard_a_valid_installed_checkpoint() {
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(VALID_CANDIDATE),
        ScriptedModelProviderResponse::SetupError(invalid_request("second pass rejected")),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime = runtime_with_compactor_and_steps(
        "compaction-failed-later-pass",
        compactor.clone(),
        32_000,
        6,
    );
    seed_rolling_history(&runtime).await;
    let policy = CitationCompactionPolicy::new(Some(10_000), Some(99_999), 1).expect("policy");
    assert!(matches!(
        runtime.compact_context_once(policy, StepContext::default()).await,
        Err(RuntimeError::CompactionModelSetup { message }) if message.contains("second pass rejected")
    ));
    assert!(runtime.compacted_checkpoint_summary().await.is_some());
    assert_eq!(compactor.recorded_requests().len(), 2);
    runtime
        .compact_context_once(policy, StepContext::default())
        .await
        .expect("retry resumes from the committed checkpoint")
        .expect("remaining history compacted");
    assert_eq!(compactor.recorded_requests().len(), 3);
}
