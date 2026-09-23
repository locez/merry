use crate::{
    CitationCompactionPolicy, CompactionConfig, FileSessionStore, Runtime, RuntimeModelRole,
    StepContext,
    runtime::tests::support::{
        common::{collect_step, completed_event_with, model_name, named_model, session_id},
        model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
    },
    token_estimate::{estimate_model_input_tokens, estimate_request_input_tokens},
};
use merry_core::{ModelUsage, ProviderName, RuntimeJournalPayload};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelOutput,
    ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse, ModelStreamContext,
    ProviderErrorKind,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Default)]
enum Feedback {
    #[default]
    Measured,
    Missing,
    Zero,
    Cancelled,
    Failed,
}

struct MeteredProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    requests: Mutex<Vec<ModelRequest>>,
    feedback: Mutex<VecDeque<Feedback>>,
}

impl MeteredProvider {
    fn new(feedback: Vec<Feedback>) -> Self {
        Self {
            name: ProviderName::new("metered-provider").expect("provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, Some(64_000), Some(512))
                .expect("capabilities"),
            requests: Mutex::new(Vec::new()),
            feedback: Mutex::new(feedback.into()),
        }
    }

    fn last_request(&self) -> ModelRequest {
        self.requests
            .lock()
            .expect("requests")
            .last()
            .expect("request")
            .clone()
    }
}

impl ModelProvider for MeteredProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            let base_tokens = estimate_request_input_tokens(&request);
            self.requests.lock().expect("requests").push(request);
            let feedback = self
                .feedback
                .lock()
                .expect("feedback")
                .pop_front()
                .unwrap_or_default();
            if matches!(feedback, Feedback::Failed) {
                return Err(ModelError::provider(
                    ProviderErrorKind::InvalidRequest,
                    "fixture failure",
                ));
            }
            let actual_tokens = match feedback {
                Feedback::Cancelled => base_tokens * 100,
                Feedback::Zero => 0,
                _ => base_tokens * 2,
            };
            let usage =
                (!matches!(feedback, Feedback::Missing)).then_some(ModelUsage::with_details(
                    actual_tokens,
                    Some(actual_tokens),
                    1,
                    None,
                    actual_tokens + 1,
                ));
            let finish = if matches!(feedback, Feedback::Cancelled) {
                FinishReason::Cancelled
            } else {
                FinishReason::Stop
            };
            let stream: ModelEventStream =
                Box::pin(futures_util::stream::iter([Ok(ModelEvent::Completed {
                    response: ModelResponse::new(vec![ModelOutput::text("done")], finish, usage),
                })]));
            Ok(stream)
        })
    }
}

fn compactor() -> RecordingModelProvider {
    let candidate = r#"{
        "confirmed_decisions": [], "rejected_approaches": [],
        "constraints_preferences_boundaries": [], "corrected_misunderstandings": [],
        "durable_conclusions": [{"id":"c1", "text":"Earlier history was compacted.", "refs":["h0"]}],
        "open_questions": [], "current_progress_and_next_steps": [], "exact_details": [], "handoffs": []
    }"#;
    RecordingModelProvider::with_script_and_capabilities(
        (0..4)
            .map(|_| {
                ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                    vec![ModelOutput::text(candidate)],
                    FinishReason::Stop,
                ))])
            })
            .collect(),
        ModelCapabilities::new(true, true, false, true, Some(256_000), None).expect("capabilities"),
    )
}

fn policy() -> CitationCompactionPolicy {
    CitationCompactionPolicy::new(Some(512), Some(16_384), 1).expect("policy")
}

async fn assert_prediction(runtime: &Runtime, provider: &MeteredProvider, multiplier: u64) {
    let usage = runtime.usage().await.expect("usage");
    let request = provider.last_request();
    assert_eq!(
        usage.last.input_tokens(),
        estimate_request_input_tokens(&request) * 2
    );
    assert_eq!(
        usage.last.cached_input_tokens(),
        Some(usage.last.input_tokens())
    );
    let compaction = usage.compaction.expect("compaction snapshot");
    assert_eq!(
        compaction.dynamic_body_estimated_tokens,
        Some(estimate_model_input_tokens(request.dynamic_input()) * multiplier),
    );
}

#[tokio::test]
async fn multi_turn_feedback_triggers_compaction_before_the_uncalibrated_estimate_would() {
    for feedback in [Feedback::Missing, Feedback::Measured] {
        let primary = Arc::new(MeteredProvider::new(vec![feedback; 12]));
        let compactor = compactor();
        let runtime = Runtime::builder(session_id("calibration-multi-turn"))
            .model_provider(primary.clone(), model_name())
            .model_provider_for_role(
                RuntimeModelRole::ContextCompaction,
                Arc::new(compactor.clone()),
                named_model("compactor"),
            )
            .automatic_compaction(CompactionConfig::enabled(policy()))
            .build()
            .expect("runtime");
        let mut compacted = false;
        for turn in 0..12 {
            let text = format!("Turn {turn}: {}", "abcd".repeat(3_000));
            let events = collect_step(&runtime, &text, StepContext::default()).await;
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
                "turn {turn}: {events:?}"
            );
            compacted |= events.iter().any(|event| {
                matches!(
                    event.payload,
                    RuntimeJournalPayload::CompactionCompleted { .. }
                )
            });
            if matches!(feedback, Feedback::Measured) {
                assert_prediction(&runtime, &primary, if turn == 0 { 1 } else { 2 }).await;
            }
        }
        assert_eq!(compacted, matches!(feedback, Feedback::Measured));
        assert_eq!(compactor.recorded_requests().is_empty(), !compacted);
    }
}

#[tokio::test]
async fn calibrated_retention_planning_fits_the_tail_before_installing_a_checkpoint() {
    for (feedback, expected_covered_items) in [(Feedback::Missing, 4), (Feedback::Measured, 6)] {
        let primary = Arc::new(MeteredProvider::new(vec![feedback; 4]));
        let compactor = compactor();
        let runtime = Runtime::builder(session_id("calibrated-retention"))
            .model_provider(primary, model_name())
            .model_provider_for_role(
                RuntimeModelRole::ContextCompaction,
                Arc::new(compactor.clone()),
                named_model("compactor"),
            )
            .automatic_compaction(CompactionConfig::disabled())
            .build()
            .expect("runtime");
        for turn in 0..4 {
            let events = collect_step(
                &runtime,
                &format!("Turn {turn}: {}", "abcd".repeat(3_000)),
                StepContext::default(),
            )
            .await;
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
            );
        }
        let outcome = runtime
            .compact_context_once(
                policy()
                    .with_retained_model_turns(2)
                    .expect("retention policy"),
                StepContext::default(),
            )
            .await
            .expect("manual compaction")
            .expect("checkpoint");
        assert_eq!(outcome.covered_history_item_count(), expected_covered_items);
        assert_eq!(compactor.recorded_requests().len(), 1);
    }
}

#[tokio::test]
async fn manual_compaction_and_resumed_requests_keep_the_primary_calibration() {
    let primary = Arc::new(MeteredProvider::new(Vec::new()));
    let compactor = compactor();
    let runtime = Runtime::builder(session_id("calibration-manual-resume"))
        .model_provider(primary.clone(), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            named_model("compactor"),
        )
        .automatic_compaction(CompactionConfig::disabled())
        .build()
        .expect("runtime");
    for turn in 0..8 {
        let events = collect_step(
            &runtime,
            &format!("Turn {turn}: {}", "abcd".repeat(3_000)),
            StepContext::default(),
        )
        .await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
        );
    }
    runtime
        .compact_context_once(policy(), StepContext::default())
        .await
        .expect("manual compaction")
        .expect("checkpoint");
    assert_eq!(
        compactor.recorded_requests().len(),
        1,
        "retained history must be fitted in calibrated units"
    );
    let directory = tempfile::tempdir().expect("store directory");
    let store = FileSessionStore::new(directory.path());
    runtime.save_session_to(store.clone()).await.expect("save");
    let resumed = Runtime::builder(runtime.session_id().clone())
        .model_provider(primary.clone(), model_name())
        .automatic_compaction(CompactionConfig::disabled())
        .resume_from_store(store.clone())
        .await
        .expect("resume");
    let events = collect_step(
        &resumed,
        "Continue after checkpoint and resume.",
        StepContext::default(),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    assert_prediction(&resumed, &primary, 2).await;
    let usage = resumed
        .usage()
        .await
        .expect("usage")
        .compaction
        .expect("budget");
    assert!(usage.dynamic_body_estimated_tokens.expect("estimate") < usage.hard_water_tokens);

    let switched = Runtime::builder(runtime.session_id().clone())
        .model_provider(primary.clone(), named_model("different-model"))
        .automatic_compaction(CompactionConfig::disabled())
        .resume_from_store(store)
        .await
        .expect("resume with another model");
    let events = collect_step(
        &switched,
        "New model must start without inherited feedback.",
        StepContext::default(),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    assert_prediction(&switched, &primary, 1).await;
}

#[tokio::test]
async fn missing_zero_cancelled_and_failed_usage_do_not_replace_valid_feedback() {
    let primary = Arc::new(MeteredProvider::new(vec![
        Feedback::Measured,
        Feedback::Missing,
        Feedback::Zero,
        Feedback::Cancelled,
        Feedback::Failed,
        Feedback::Measured,
    ]));
    let runtime = Runtime::builder(session_id("calibration-invalid-feedback"))
        .model_provider(primary.clone(), model_name())
        .build()
        .expect("runtime");
    for _ in 0..5 {
        collect_step(&runtime, "Previous request.", StepContext::default()).await;
    }
    let events = collect_step(
        &runtime,
        "Use the last valid calibration.",
        StepContext::default(),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "{events:?}"
    );
    assert_prediction(&runtime, &primary, 2).await;
}
