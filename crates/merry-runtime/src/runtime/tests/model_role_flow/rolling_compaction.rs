use crate::{
    CitationCompactionPolicy, RuntimeModelRole, StepContext,
    artifact::ArtifactContent,
    runtime::{
        CompactionConfig, Runtime,
        tests::support::{
            common::{
                artifact_id, collect_step, completed_event, completed_event_with, model_name,
                pending_tool_call, session_id,
            },
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
        },
    },
};
use merry_core::{
    ArtifactKind, ArtifactRef, PendingToolCallBatch, RuntimeJournalPayload, ToolCallBatchId,
    ToolCallResult,
};
use merry_llm::{FinishReason, ModelCapabilities, ModelName, ModelOutput, ModelProvider};
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
        // One candidate per allowed pass, so exhausting the script would fail the
        // test rather than silently falling back to a non-candidate response.
        (0..12)
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
    assert!(
        requests.len() <= 12,
        "passes must stay inside the rolling bound, got {}",
        requests.len()
    );
}

/// A window that shrank far below the history is reduced in one pass.
///
/// The history is dominated by tool results, which is the case one-shot exists for:
/// rolling would send every covered result at full length and need several passes,
/// re-summarizing the previous checkpoint each time. One-shot covers the whole
/// history once and shortens the older results, so the step finishes after a single
/// compaction call and the payload still names every covered exchange.
#[tokio::test(flavor = "current_thread")]
async fn automatic_compaction_covers_everything_once_when_the_window_shrinks_far() {
    assert_one_shot_reduction(64_000, 25_200, 10_000, 5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_uses_request_fit_below_the_old_one_and_a_half_window_threshold() {
    assert_one_shot_reduction(64_000, 14_000, 10_000, 5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_reduces_700k_history_after_switching_from_1m_to_272k() {
    assert_one_shot_reduction(272_000, 140_000, 40_000, 5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn rebuilt_request_keeps_four_tool_exchanges_when_five_do_not_fit() {
    for retained_tools in [5, 1000] {
        let request = assert_one_shot_reduction(64_000, 48_000, 10_000, retained_tools).await;
        let text = request
            .messages()
            .last()
            .expect("directive")
            .content()
            .as_text();
        let payload = text
            .split_once("<merry_compaction_payload>\n")
            .expect("payload start")
            .1
            .split_once("\n</merry_compaction_payload>")
            .expect("payload end")
            .0;
        let payload: serde_json::Value = serde_json::from_str(payload).expect("payload");
        let exchanges = payload["window"]
            .as_array()
            .expect("window")
            .iter()
            .flat_map(|turn| turn["items"].as_array().expect("turn items"))
            .filter(|item| item["role"] == "tool_exchange")
            .count();
        assert_eq!(
            exchanges, 4,
            "keep the largest affordable number, not a halved count"
        );
    }
}

async fn assert_one_shot_reduction(
    window_tokens: u64,
    result_bytes: usize,
    max_installed_tokens: u64,
    retained_tool_exchanges: usize,
) -> merry_llm::ModelRequest {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        (0..40)
            .map(|_| ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]))
            .collect(),
        ModelCapabilities::new(true, true, false, true, Some(1_000_000), None)
            .expect("valid primary capabilities"),
    );
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        (0..12)
            .map(|_| {
                ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                    vec![ModelOutput::text(ROLLING_CANDIDATE)],
                    FinishReason::Stop,
                ))])
            })
            .collect(),
        ModelCapabilities::new(true, true, false, true, Some(window_tokens), None)
            .expect("valid compactor capabilities"),
    );
    let runtime = Runtime::builder(session_id("one-shot-compaction-window-shrink"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/one-shot-compactor").expect("valid model"),
        )
        .automatic_compaction(CompactionConfig::enabled(
            CitationCompactionPolicy::new(None, None, 5)
                .expect("valid policy")
                .with_one_shot_retained_tool_exchanges(retained_tool_exchanges),
        ))
        .build()
        .expect("runtime should build");

    // Twenty tool turns whose results dominate the body, sized so the body lands
    // above one and a half windows once the window shrinks to 64k.
    {
        let mut session = runtime.inner.session.lock().await;
        for index in 1..=20 {
            let turn_id = session.begin_model_turn().expect("tool turn begins");
            session
                .record_user_message_body(turn_id, &format!("one-shot turn {index:02}"))
                .expect("tool user message records");
            let call = pending_tool_call(&format!("one-shot-call-{index:02}"));
            session
                .record_tool_call_batch_pending(
                    turn_id,
                    PendingToolCallBatch::new(
                        ToolCallBatchId::new(&format!("one-shot-batch-{index:02}"))
                            .expect("valid batch id"),
                        vec![call.clone()],
                    )
                    .expect("valid tool batch"),
                )
                .expect("tool call records");
            session
                .close_model_response(turn_id, true)
                .expect("tool response closes");
            session
                .submit_tool_result(
                    ToolCallResult::succeeded(
                        call.id().clone(),
                        ArtifactRef::new(
                            artifact_id(&format!("one-shot-result-{index:02}")),
                            ArtifactKind::Text,
                        ),
                    ),
                    ArtifactContent::text(format!(
                        "one-shot result body {index} {}",
                        "z".repeat(result_bytes)
                    )),
                )
                .expect("tool result records");
        }
    }

    runtime
        .update_interactive_context_window_tokens(NonZeroU64::new(window_tokens))
        .await;
    let events = collect_step(
        &runtime,
        "turn after the window shrank far",
        StepContext::default(),
    )
    .await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "the step must complete after one-shot compaction: {:?}",
        events.last().map(|event| &event.payload)
    );
    let requests = compactor.recorded_requests();
    assert_eq!(
        requests.len(),
        1,
        "one-shot reduces the history in one pass, got {}",
        requests.len()
    );
    let payload = requests[0]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        payload.contains("one-shot turn 01"),
        "the single pass must cover the oldest covered turn's text"
    );
    assert!(
        !payload.contains("one-shot turn 20"),
        "the retained tail stays in the conversation, not the payload"
    );
    // Covered tool exchanges outside the newest five are omitted entirely, leaving
    // no call id, artifact id, or marker behind. On the measured session the covered
    // exchanges were 225,800 tokens of arguments and 47,800 tokens of results against
    // a 190,400 token input budget, so nothing per exchange could stay.
    assert!(
        !payload.contains("one-shot-call-01"),
        "an omitted covered exchange must not appear in the payload"
    );
    assert!(
        !payload.contains("one-shot-result-01"),
        "an omitted covered exchange must not name its artifact either"
    );
    assert!(
        payload.contains("one-shot-call-15"),
        "the newest covered exchanges are retained whole"
    );
    let (input_tokens, output_tokens) =
        crate::compaction::compaction_request_required_tokens(&requests[0]);
    assert!(input_tokens + output_tokens <= window_tokens);
    let final_requests = primary.recorded_requests();
    let final_request = final_requests.last().expect("primary continuation");
    assert_eq!(requests[0].tools(), final_request.tools());
    let primary_budget = crate::runtime::provider_request::request_context_budget(
        primary.capabilities(),
        final_request,
        Some(window_tokens),
    )
    .expect("primary budget");
    assert!(
        primary_budget.dynamic_body_estimated_tokens < max_installed_tokens,
        "one reduction must meet the destination-sized target: {} >= {max_installed_tokens}",
        primary_budget.dynamic_body_estimated_tokens
    );
    requests[0].clone()
}
