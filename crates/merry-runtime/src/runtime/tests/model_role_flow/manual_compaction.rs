use crate::{
    CitationCompactionPolicy, RuntimeModelRole, StepContext,
    runtime::{
        Runtime,
        tests::{
            model_role_flow::seed_two_history_items_for_compaction,
            support::{
                common::{completed_event, completed_event_with, model_name, session_id},
                model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
            },
        },
    },
};
use merry_llm::{FinishReason, ModelCapabilities, ModelEvent, ModelName, ModelOutput};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn compaction_uses_context_compaction_role_when_configured() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ],
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("valid primary capabilities"),
    );
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text(
                    r#"{
                      "confirmed_decisions": [],
                      "rejected_approaches": [],
                      "constraints_preferences_boundaries": [],
                      "corrected_misunderstandings": [],
                      "durable_conclusions": [
                        {
                          "id": "c1",
                          "text": "Old history was compacted.",
                          "refs": ["h0", "h1"]
                        }
                      ],
                      "open_questions": [],
                      "current_progress_and_next_steps": [],
                      "exact_details": [],
                      "handoffs": []
                    }"#,
                )],
                FinishReason::Stop,
            ),
        )])],
        ModelCapabilities::new(true, true, false, true, Some(256_000), None)
            .expect("valid compactor capabilities"),
    );
    let runtime = Runtime::builder(session_id("compaction-role"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("compaction-model").expect("valid model"),
        )
        .build()
        .expect("runtime builds");

    seed_two_history_items_for_compaction(&runtime).await;
    let primary_before = primary.recorded_requests().len();
    let policy = CitationCompactionPolicy::new(None, None, 1).expect("valid policy");
    let prepared = runtime
        .citation_compaction_input(policy)
        .await
        .expect("manual compaction input builds")
        .expect("manual compaction input exists");
    assert_eq!(
        prepared.resolved_budget().output_token_limit(),
        5_120,
        "manual input budget must come from the 64k primary window"
    );

    let outcome = runtime
        .compact_context_once(policy, StepContext::default())
        .await
        .expect("compaction succeeds")
        .expect("compaction happened");

    assert_eq!(outcome.covered_history_item_count(), 2);
    assert_eq!(primary.recorded_requests().len(), primary_before);
    assert_eq!(compactor.recorded_requests().len(), 1);
    assert_eq!(
        compactor.recorded_requests()[0].model().as_str(),
        "compaction-model"
    );
    assert_eq!(
        compactor.recorded_requests()[0]
            .generation()
            .max_output_tokens(),
        Some(5_120),
        "manual compaction budget must come from the 64k primary window"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_uses_explicit_primary_window_override() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ],
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("valid primary capabilities"),
    );
    let compactor = RecordingModelProvider::with_script_and_capabilities(
        vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text(
                    r#"{
                      "confirmed_decisions": [],
                      "rejected_approaches": [],
                      "constraints_preferences_boundaries": [],
                      "corrected_misunderstandings": [],
                      "durable_conclusions": [
                        {
                          "id": "c1",
                          "text": "Old history was compacted with an explicit primary window.",
                          "refs": ["h0", "h1"]
                        }
                      ],
                      "open_questions": [],
                      "current_progress_and_next_steps": [],
                      "exact_details": [],
                      "handoffs": []
                    }"#,
                )],
                FinishReason::Stop,
            ),
        )])],
        ModelCapabilities::new(true, true, false, true, Some(256_000), None)
            .expect("valid compactor capabilities"),
    );
    let runtime = Runtime::builder(session_id("compaction-explicit-primary-window"))
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("compaction-model").expect("valid model"),
        )
        .build()
        .expect("runtime builds");
    runtime
        .update_interactive_context_window_tokens(std::num::NonZeroU64::new(128_000))
        .await;
    seed_two_history_items_for_compaction(&runtime).await;

    runtime
        .compact_context_once(
            CitationCompactionPolicy::new(None, None, 1).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("manual compaction succeeds")
        .expect("manual compaction runs");

    assert_eq!(
        compactor.recorded_requests()[0]
            .generation()
            .max_output_tokens(),
        Some(10_240),
        "explicit 128k primary window must override both provider windows"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_accepts_streamed_text_delta_before_completed_response() {
    let primary = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
    ]);
    let candidate_json = r#"{
          "confirmed_decisions": [],
          "rejected_approaches": [],
          "constraints_preferences_boundaries": [],
          "corrected_misunderstandings": [],
          "durable_conclusions": [
            {
              "id": "c1",
              "text": "Old history was compacted.",
              "refs": ["h0", "h1"]
            }
          ],
          "open_questions": [],
          "current_progress_and_next_steps": [],
          "exact_details": [],
          "handoffs": []
        }"#;
    let compactor =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: candidate_json.to_owned(),
            }),
            Ok(completed_event_with(
                vec![ModelOutput::text(candidate_json)],
                FinishReason::Stop,
            )),
        ])]);
    let runtime = Runtime::builder(session_id("compaction-streamed-delta"))
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor),
            ModelName::new("compaction-model").expect("valid model"),
        )
        .build()
        .expect("runtime builds");

    seed_two_history_items_for_compaction(&runtime).await;

    let outcome = runtime
        .compact_context_once(
            CitationCompactionPolicy::new(Some(128), Some(4096), 1).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("compaction accepts streamed text delta")
        .expect("compaction happened");

    assert_eq!(outcome.covered_history_item_count(), 2);
}
