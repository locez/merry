use super::*;
use crate::compaction::compaction_request_required_tokens;
use crate::token_estimate::estimate_text_tokens;
use serde_json::Value;

pub(super) fn repair_payload(request: &ModelRequest) -> Value {
    let text = request
        .messages()
        .last()
        .expect("repair message")
        .content()
        .as_text();
    let payload = text
        .split_once("<merry_compaction_repair>\n")
        .expect("feedback boundary")
        .1
        .split_once("\n</merry_compaction_repair>")
        .expect("feedback end")
        .0;
    serde_json::from_str(payload).expect("typed repair payload")
}

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
    assert!(logs.contains("\"event\":\"runtime.compaction.candidate_evaluated\""));
    assert!(logs.contains("\"soft_target_tokens\":6400"));
    assert!(logs.contains("\"hard_limit_tokens\":19200"));
    assert!(logs.contains("\"accepted\":true"));
}

#[tokio::test(flavor = "current_thread")]
async fn hard_limit_repair_preserves_request_prefix_and_reports_safe_metrics() {
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
        .expect("repair succeeds")
        .expect("checkpoint installed");
    let requests = compactor.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].input().starts_with(requests[0].input()));
    assert_eq!(requests[0].tools(), requests[1].tools());
    assert_eq!(
        requests[0].tool_profile_hash(),
        requests[1].tool_profile_hash()
    );
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash()
    );
    assert_eq!(requests[0].generation(), requests[1].generation());
    assert_eq!(requests[0].response_format(), requests[1].response_format());
    let payload = repair_payload(&requests[1]);
    assert_eq!(payload["reason"], "rendered_summary_too_large");
    assert_eq!(payload["measurements"]["hard_limit_tokens"], 512);
    assert!(
        payload["measurements"]["rendered_summary_tokens"]
            .as_u64()
            .expect("tokens")
            > 512
    );
    assert_eq!(payload["rejected_candidate"], oversized);
    assert!(
        !logs.contains(secret_marker),
        "numeric diagnostics must not log candidate bodies"
    );
    let (input, output) = compaction_request_required_tokens(&requests[1]);
    assert!(input + output <= 64_000);
}

#[tokio::test(flavor = "current_thread")]
async fn keep_expansion_is_budgeted_and_repair_can_replace_an_oversized_old_summary() {
    let old = VALID_CANDIDATE.replace(
        "Old history was compacted.",
        &"old checkpoint detail ".repeat(1000),
    );
    let mut keep: Value = serde_json::from_str(VALID_CANDIDATE).expect("candidate");
    keep["durable_conclusions"] = serde_json::json!([]);
    keep["handoffs"] =
        serde_json::json!([{"action":"keep", "old_id":"c1", "new_ids":null, "reason":null}]);
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(&old),
        completed_candidate(&keep.to_string()),
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
    result
        .expect("rewrite after failed keep")
        .expect("installed");
    let requests = compactor.recorded_requests();
    assert_eq!(requests.len(), 3);
    let original = requests[1]
        .messages()
        .last()
        .expect("compaction directive")
        .content()
        .as_text();
    let original = original
        .split_once("<merry_compaction_payload>\n")
        .expect("start")
        .1
        .split_once("\n</merry_compaction_payload>")
        .expect("end")
        .0;
    let payload: Value = serde_json::from_str(original).expect("payload");
    let old_tokens = payload["previous_checkpoint"]["estimated_tokens"]
        .as_u64()
        .expect("old size");
    assert!(old_tokens > 512);
    let feedback = repair_payload(&requests[2]);
    assert_eq!(feedback["measurements"]["kept_entry_count"], 1);
    assert_eq!(
        feedback["measurements"]["rendered_summary_tokens"],
        old_tokens
    );
    assert_eq!(
        feedback["measurements"]["previous_summary_tokens"],
        old_tokens
    );
    assert!(
        feedback["measurements"]["kept_entry_tokens"]
            .as_u64()
            .expect("kept cost")
            > 512
    );
    assert!(logs.contains("\"kept_entry_count\":1"));
    let summary = crate::ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("repaired context")
        .to_snapshot();
    assert!(estimate_text_tokens(&summary) <= 512);
    assert!(!summary.contains("old checkpoint detail"));
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_oversize_never_relaxes_an_explicit_hard_limit_or_installs_a_candidate() {
    let oversized = VALID_CANDIDATE.replace("Old history was compacted.", &"x".repeat(8_000));
    let compactor = RecordingModelProvider::with_script(vec![
        completed_candidate(&oversized),
        completed_candidate(&oversized),
    ]);
    let runtime = runtime_with_compactor("explicit-hard-budget", compactor.clone(), 128_000);
    seed_two_history_items_for_compaction(&runtime).await;
    let error = runtime
        .compact_context_once(compaction_policy(), StepContext::default())
        .await
        .expect_err("hard ceiling remains authoritative after repair");
    assert!(matches!(
        error,
        RuntimeError::Compaction {
            source: crate::CompactionError::RenderedCheckpointTooLarge {
                max_tokens: 512,
                ..
            }
        }
    ));
    assert_eq!(compactor.recorded_requests().len(), 2);
    assert!(runtime.compacted_checkpoint_summary().await.is_none());
}
