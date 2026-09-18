use super::*;
use crate::runtime::tests::support::common::collect_step;
use crate::{CompactionConfig, CompactionError, RuntimeError};
use merry_core::RuntimeJournalPayload;
use merry_llm::ModelMessageRole;

fn runtime_with_tail_budget(
    name: &str,
) -> (Runtime, RecordingModelProvider, RecordingModelProvider) {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        (0..7)
            .map(|_| {
                ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                    vec![ModelOutput::text("completed answer")],
                    FinishReason::Stop,
                ))])
            })
            .collect(),
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("primary capabilities"),
    );
    let candidate = serde_json::json!({
        "confirmed_decisions": [],
        "rejected_approaches": [],
        "constraints_preferences_boundaries": [],
        "corrected_misunderstandings": [],
        "durable_conclusions": [{"id": "c1", "text": "Covered history.", "refs": ["h0"]}],
        "open_questions": [],
        "current_progress_and_next_steps": [],
        "exact_details": [],
        "handoffs": []
    });
    let compactor =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text(&candidate.to_string())],
                FinishReason::Stop,
            ),
        )])]);
    let runtime = Runtime::builder(session_id(name))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("compactor").expect("model"),
        )
        .automatic_compaction(CompactionConfig::disabled())
        .build()
        .expect("runtime");
    (runtime, primary, compactor)
}

async fn seed_turns(runtime: &Runtime, body_bytes: usize) -> Vec<String> {
    let mut messages = Vec::new();
    for turn in 1..=6 {
        let message = format!("turn {turn}: {}", "x".repeat(body_bytes));
        let events = collect_step(runtime, &message, StepContext::default()).await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
        );
        messages.push(message);
    }
    messages
}

#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_and_preview_keep_four_complete_pairs_when_five_exceed_tail_budget() {
    let (runtime, primary, compactor) = runtime_with_tail_budget("manual-tail-four");
    let messages = seed_turns(&runtime, 6_000).await;
    let policy = CitationCompactionPolicy::default();
    let preview = runtime
        .citation_compaction_input(policy)
        .await
        .expect("preview fits")
        .expect("covered history");
    assert_eq!(
        preview.window_plan().retained_turn_ids_u64(),
        vec![3, 4, 5, 6]
    );

    let outcome = runtime
        .compact_context_once(policy, StepContext::default())
        .await
        .expect("compaction fits")
        .expect("installed");
    assert_eq!(outcome.covered_history_item_count(), 4);
    assert_eq!(compactor.recorded_requests().len(), 1);
    collect_step(&runtime, "continue", StepContext::default()).await;
    let requests = primary.recorded_requests();
    let final_request = requests.last().expect("resumed primary request");
    let raw_users: Vec<_> = final_request
        .messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::User)
        .map(|message| message.content().as_text())
        .collect();
    assert_eq!(
        raw_users,
        messages[2..]
            .iter()
            .map(String::as_str)
            .chain(["continue"])
            .collect::<Vec<_>>()
    );
    assert_eq!(
        final_request
            .messages()
            .iter()
            .filter(|message| message.role() == ModelMessageRole::Assistant)
            .count(),
        4
    );
}

#[tokio::test(flavor = "current_thread")]
async fn manual_tail_above_soft_target_keeps_one_pair_instead_of_filling_the_window() {
    let (runtime, _, compactor) = runtime_with_tail_budget("manual-tail-soft-fallback");
    seed_turns(&runtime, 4_000).await;
    collect_step(&runtime, &"x".repeat(56_000), StepContext::default()).await;
    let preview = runtime
        .citation_compaction_input(CitationCompactionPolicy::default())
        .await
        .expect("one pair fits the hard budget")
        .expect("history is compressible");
    assert_eq!(preview.window_plan().retained_turn_ids_u64(), vec![7]);
    runtime
        .compact_context_once(CitationCompactionPolicy::default(), StepContext::default())
        .await
        .expect("an indivisible tail below the hard limit is acceptable")
        .expect("checkpoint installed");
    assert_eq!(compactor.recorded_requests().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_rejects_a_tail_that_cannot_fit_before_calling_the_model() {
    let (runtime, _, compactor) = runtime_with_tail_budget("manual-tail-hard-failure");
    for message in ["older history", &"x".repeat(240_000)] {
        collect_step(&runtime, message, StepContext::default()).await;
    }
    let policy = CitationCompactionPolicy::default();
    for result in [
        runtime.citation_compaction_input(policy).await.map(|_| ()),
        runtime
            .compact_context_once(policy, StepContext::default())
            .await
            .map(|_| ()),
    ] {
        assert!(matches!(
            result,
            Err(RuntimeError::Compaction {
                source: CompactionError::MinimumRawTurnCannotFit
            })
        ));
    }
    assert!(compactor.recorded_requests().is_empty());
    assert!(runtime.compacted_checkpoint_summary().await.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn automatic_compaction_accepts_an_indivisible_tail_below_the_hard_limit() {
    let (runtime, primary, compactor) = runtime_with_tail_budget("automatic-tail-soft-fallback");
    collect_step(&runtime, &"x".repeat(180_000), StepContext::default()).await;
    let retained = "y".repeat(56_000);
    collect_step(&runtime, &retained, StepContext::default()).await;
    runtime
        .update_interactive_automatic_compaction(CompactionConfig::enabled(
            CitationCompactionPolicy::default(),
        ))
        .await;
    let events = collect_step(&runtime, "continue", StepContext::default()).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "{events:?}"
    );
    assert_eq!(compactor.recorded_requests().len(), 1);
    let requests = primary.recorded_requests();
    let users: Vec<_> = requests
        .last()
        .expect("continued request")
        .messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::User)
        .map(|message| message.content().as_text())
        .collect();
    assert_eq!(users, vec![retained.as_str(), "continue"]);
}
