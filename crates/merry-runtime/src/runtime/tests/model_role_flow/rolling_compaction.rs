use crate::{
    CitationCompactionPolicy, RuntimeModelRole, StepContext,
    runtime::{
        CompactionConfig, Runtime,
        tests::support::{
            common::{collect_step, completed_event, completed_event_with, model_name, session_id},
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
        },
    },
};
use merry_core::RuntimeJournalPayload;
use merry_llm::{FinishReason, ModelCapabilities, ModelName, ModelOutput};
use std::num::NonZeroU64;
use std::sync::Arc;

const ROLLING_CANDIDATE: &str = r#"{
  "confirmed_decisions": [],
  "rejected_approaches": [],
  "constraints_preferences_boundaries": [],
  "corrected_misunderstandings": [],
  "durable_conclusions": [
    {
      "id": "c1",
      "text": "Seeded history was reduced by a rolling pass.",
      "refs": ["h0"]
    }
  ],
  "open_questions": [],
  "current_progress_and_next_steps": [],
  "exact_details": [],
  "handoffs": []
}"#;

/// History seeded under a wide window still compacts after the window shrinks.
///
/// This is the case that reported "no compaction window fits the compaction request
/// budget": the history was collected while the context window was wide, then the
/// window shrank below it. One reduction can only cover what the compaction request
/// can host, so the step has to run more than one reduction before the recompiled
/// request fits the new watermark.
#[tokio::test(flavor = "current_thread")]
async fn automatic_compaction_rolls_when_the_window_shrinks_below_the_history() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        (0..70)
            .map(|_| ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]))
            .collect(),
        ModelCapabilities::new(true, true, false, true, Some(1_000_000), None)
            .expect("valid primary capabilities"),
    );
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        (0..8)
            .map(|_| {
                ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                    vec![ModelOutput::text(ROLLING_CANDIDATE)],
                    FinishReason::Stop,
                ))])
            })
            .collect(),
        // The compaction model matches the shrunken window, so one request can only
        // cover a window-worth of history.
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("valid compactor capabilities"),
    );
    let runtime = Runtime::builder(session_id("rolling-compaction-window-shrink"))
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/rolling-compactor").expect("valid model"),
        )
        .automatic_compaction(CompactionConfig::enabled(
            CitationCompactionPolicy::new(None, None, 5).expect("valid policy"),
        ))
        .build()
        .expect("runtime should build");

    for index in 0..60 {
        collect_step(
            &runtime,
            &format!("seed turn {index} {}", "y".repeat(8_000)),
            StepContext::default(),
        )
        .await;
    }
    assert!(
        compactor.recorded_requests().is_empty(),
        "a window wide enough for the history must not compact"
    );

    runtime
        .update_interactive_context_window_tokens(NonZeroU64::new(64_000))
        .await;
    let events = collect_step(
        &runtime,
        "final turn after the window shrank",
        StepContext::default(),
    )
    .await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "the step must complete after rolling compaction: {:?}",
        events.last().map(|event| &event.payload)
    );
    let requests = compactor.recorded_requests();
    assert!(
        requests.len() >= 2,
        "a shrunken window needs more than one reduction, got {}",
        requests.len()
    );
}
