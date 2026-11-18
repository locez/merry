use super::*;
use crate::compaction::compaction_request_required_tokens;
use crate::token_estimate::estimate_text_tokens;

#[tokio::test(flavor = "current_thread")]
async fn window_128k_accepts_summary_above_soft_target_without_retry() {
    let candidate = VALID_CANDIDATE.replace("Old history was compacted.", &"x".repeat(34_000));
    let compactor = RecordingModelProvider::with_script(vec![completed_candidate(&candidate)]);
    let runtime = runtime_with_compactor("soft-budget-128k", compactor.clone(), 128_000);
    seed_two_history_items_for_compaction(&runtime).await;
    let policy = CitationCompactionPolicy::default()
        .with_retained_model_turns(1)
        .expect("policy");
    let (result, logs) = capture_traces_for(
        "soft-budget-128k",
        runtime.compact_context_once(policy, StepContext::default()),
    )
    .await;
    result
        .expect("soft excess is acceptable")
        .expect("checkpoint installed");
    let summary = crate::ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("installed context")
        .to_snapshot();
    let tokens = estimate_text_tokens(&summary);
    assert!(tokens > 6_400 && tokens < 19_200);
    assert_eq!(compactor.recorded_requests().len(), 1);
    assert!(logs.contains("\"event\":\"runtime.compaction.summary_evaluated\""));
    assert!(logs.contains("\"soft_target_tokens\":6400"));
    assert!(logs.contains("\"output_limit_tokens\":19200"));
    assert!(logs.contains("\"accepted\":true"));
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_prose_installs_in_one_call_without_logging_the_body() {
    let secret_marker = "private-candidate-content-not-for-logs";
    let oversized =
        VALID_CANDIDATE.replace("Old history was compacted.", &secret_marker.repeat(150));
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(&oversized),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime = runtime_with_compactor("hard-budget-repair", compactor.clone(), 64_000);
    seed_two_history_items_for_compaction(&runtime).await;
    let (result, logs) = capture_traces_for(
        "hard-budget-repair",
        runtime.compact_context_once(compaction_policy(), StepContext::default()),
    )
    .await;
    result
        .expect("usable prose installs")
        .expect("checkpoint installed");
    let requests = compactor.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .messages()
            .last()
            .expect("summary instruction")
            .content()
            .as_text()
            .contains("1. Primary Request and Intent:")
    );
    assert!(
        !logs.contains(secret_marker),
        "diagnostics must not log candidate bodies"
    );
    let (input, output) = compaction_request_required_tokens(&requests[0]);
    assert!(input + output <= 64_000);
}

#[tokio::test(flavor = "current_thread")]
async fn a_later_compaction_replaces_the_previous_prose_summary() {
    let old = VALID_CANDIDATE.replace(
        "Old history was compacted.",
        &"old checkpoint detail ".repeat(1000),
    );
    let replacement = "1. Primary Request and Intent: continue the auth fix\n9. Optional Next Step: run the runtime tests";
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(&old),
        completed_candidate(replacement),
        completed_candidate(VALID_CANDIDATE),
    ]);
    let runtime =
        runtime_with_compactor_and_steps("keep-budget-repair", compactor.clone(), 128_000, 3);
    seed_two_history_items_for_compaction(&runtime).await;
    runtime
        .compact_context_once(
            CitationCompactionPolicy::new(Some(10_000), None, 1).expect("old policy"),
            StepContext::default(),
        )
        .await
        .expect("old summary")
        .expect("installed");
    collect_step(
        &runtime,
        "new turn after the earlier checkpoint",
        StepContext::default(),
    )
    .await;
    let (result, logs) = capture_traces_for(
        "keep-budget-repair",
        runtime.compact_context_once(compaction_policy(), StepContext::default()),
    )
    .await;
    result.expect("later summary installs").expect("installed");
    let requests = compactor.recorded_requests();
    assert_eq!(requests.len(), 2);
    let second = serde_json::to_string(requests[1].input()).expect("request serializes");
    assert!(
        second.contains("old checkpoint detail"),
        "the next compaction sees the previous continuation"
    );
    assert!(logs.contains("\"event\":\"runtime.compaction.summary_evaluated\""));
    assert!(logs.contains("\"accepted\":true"));
    let summary = crate::ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("replaced context")
        .to_snapshot();
    assert!(summary.contains("continue the auth fix"));
    assert!(!summary.contains("old checkpoint detail"));
}

#[tokio::test(flavor = "current_thread")]
async fn an_explicit_output_budget_sizes_generation_but_does_not_reject_prose() {
    let oversized = VALID_CANDIDATE.replace("Old history was compacted.", &"x".repeat(8_000));
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(&oversized),
        completed_candidate(&oversized),
    ]);
    let runtime = runtime_with_compactor("explicit-hard-budget", compactor.clone(), 128_000);
    seed_two_history_items_for_compaction(&runtime).await;
    runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect("prose installs")
        .expect("checkpoint installed");
    let requests = compactor.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].generation().max_output_tokens().is_some());
    assert!(runtime.compacted_checkpoint_summary().await.is_some());
}
