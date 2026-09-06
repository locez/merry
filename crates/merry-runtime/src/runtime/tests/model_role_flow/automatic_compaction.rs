use crate::{
    CitationCompactionPolicy, RuntimeModelRole, StepContext,
    artifact::ArtifactContent,
    runtime::{
        AutomaticCompactionConfig, Runtime,
        tests::{
            model_role_flow::{
                TIGHT_WINDOW_OUTPUT_CAP_TOKENS, seed_two_history_items_for_compaction,
            },
            support::{
                common::{
                    artifact_id, collect_step, completed_event, completed_event_with,
                    event_kind_names, model_name, pending_tool_call, session_id,
                },
                model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
            },
        },
    },
    session::{ModelTurnId, ModelTurnStatus},
};
use merry_core::{
    ArtifactKind, ArtifactRef, PendingToolCallBatch, RuntimeJournalPayload, ToolCallBatchId,
    ToolCallResult,
};
use merry_llm::{FinishReason, ModelCapabilities, ModelName, ModelOutput};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn hard_watermark_auto_compaction_emits_lifecycle_events() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ],
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("valid capabilities"),
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
                          "text": "Old history was compacted for UI lifecycle visibility.",
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
    let automatic_compaction = AutomaticCompactionConfig::enabled(
        CitationCompactionPolicy::new(None, None, 1).expect("valid policy"),
    );
    let runtime = Runtime::builder(session_id("auto-compaction-events"))
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("compaction-model").expect("valid model"),
        )
        .automatic_compaction(automatic_compaction)
        .build()
        .expect("runtime builds");

    *runtime.inner.automatic_compaction.write().await = AutomaticCompactionConfig::disabled();
    for seed in [
        format!("Old compressible ballast.\n{}", "ballast ".repeat(24_000)),
        format!("Retained tail ballast.\n{}", "tail ".repeat(6_400)),
    ] {
        let events = collect_step(&runtime, &seed, StepContext::default()).await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
            "seed step should complete"
        );
    }
    *runtime.inner.automatic_compaction.write().await = automatic_compaction;

    let events = collect_step(
        &runtime,
        "Trigger automatic compaction with a small current input.",
        StepContext::default(),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        [
            "StepStarted",
            "CompactionStarted",
            "CompactionCompleted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    assert!(matches!(
        events[2].payload,
        RuntimeJournalPayload::CompactionCompleted {
            ref checkpoint_id,
            covered_history_item_count: 2
        } if checkpoint_id.starts_with("checkpoint-auto-compaction-events-")
    ));
    assert_eq!(
        compactor.recorded_requests()[0]
            .generation()
            .max_output_tokens(),
        Some(5_120),
        "automatic compaction budget must come from the 64k primary window"
    );
    let compactor_request = &compactor.recorded_requests()[0];
    let compactor_input = compactor_request
        .input()
        .iter()
        .map(|item| serde_json::to_string(item).expect("compactor input serializes"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compactor_input.contains("Old compressible ballast."));
    assert!(!compactor_input.contains("Retained tail ballast."));
    assert!(!compactor_input.contains("Trigger automatic compaction with a small current input."));
    assert!(compactor_request.tools().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn pre_turn_auto_compaction_failure_does_not_consume_model_turn_id() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ],
        ModelCapabilities::new(
            true,
            true,
            false,
            true,
            Some(4_000),
            Some(TIGHT_WINDOW_OUTPUT_CAP_TOKENS),
        )
        .expect("valid capabilities"),
    );
    let compactor =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text("not a valid compaction candidate")],
                FinishReason::Stop,
            ),
        )])]);
    let runtime = Runtime::builder(session_id("pre-turn-compaction-failure"))
        .model_provider(Arc::new(primary), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor),
            ModelName::new("compaction-model").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(
            CitationCompactionPolicy::new(None, None, 1).expect("valid policy"),
        ))
        .build()
        .expect("runtime builds");
    seed_two_history_items_for_compaction(&runtime).await;

    let failed = collect_step(
        &runtime,
        &format!(
            "Trigger failing pre-turn compaction.\n{}",
            "ballast ".repeat(1_200)
        ),
        StepContext::default(),
    )
    .await;

    assert!(
        failed
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::Failed { .. }))
    );
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(3)),
        None,
        "pre-turn compaction failure must not allocate the next model turn"
    );

    *runtime.inner.automatic_compaction.write().await = AutomaticCompactionConfig::disabled();
    let recovered = collect_step(
        &runtime,
        "Use the still-next model turn after compaction failure.",
        StepContext::default(),
    )
    .await;
    assert!(
        recovered
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    let session = runtime.inner.session.lock().await;
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(3)),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(session.model_turn_status(ModelTurnId::new(4)), None);
}

#[tokio::test(flavor = "current_thread")]
async fn fixed_current_input_over_hard_watermark_calls_neither_provider() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event(),
        )])],
        ModelCapabilities::new(
            true,
            true,
            false,
            true,
            Some(4_000),
            Some(TIGHT_WINDOW_OUTPUT_CAP_TOKENS),
        )
        .expect("valid capabilities"),
    );
    let compactor =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event(),
        )])]);
    let runtime = Runtime::builder(session_id("auto-compaction-skip-events"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("compaction-model").expect("valid model"),
        )
        .build()
        .expect("runtime builds");

    let events = collect_step(
        &runtime,
        &format!(
            "Oversized current input with no compressible history.\n{}",
            "ballast ".repeat(1_200)
        ),
        StepContext::default(),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    let diagnostic = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::Failed { diagnostic } => Some(diagnostic),
            _ => None,
        })
        .expect("oversized fixed current input should fail");
    assert_eq!(diagnostic.code(), "auto_compaction");
    assert!(
        diagnostic
            .message()
            .contains("current input and fixed dynamic context cannot fit")
    );
    assert!(
        primary.recorded_requests().is_empty(),
        "the oversized primary request must not be sent"
    );
    assert_eq!(
        compactor.recorded_requests().len(),
        0,
        "fixed-input overflow must fail before calling the compaction model"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn hard_watermark_archives_tool_results_without_replacing_five_retained_turns() {
    let primary = RecordingModelProvider::with_script_and_capabilities(
        vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event(),
        )])],
        ModelCapabilities::new(true, true, false, true, Some(64_000), None)
            .expect("valid capabilities"),
    );
    let compactor = RecordingModelProvider::new();
    let runtime = Runtime::builder(session_id("auto-compaction-archive-only"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("compaction-model").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(
            CitationCompactionPolicy::new(None, None, 5).expect("valid policy"),
        ))
        .build()
        .expect("runtime builds");

    {
        let mut session = runtime.inner.session.lock().await;
        for index in 1..=5 {
            let turn_id = session.begin_model_turn().expect("tool turn begins");
            session
                .record_user_message_body(turn_id, &format!("tool turn {index}"))
                .expect("tool user message records");
            let call = pending_tool_call(&format!("archive-call-{index}"));
            session
                .record_tool_call_batch_pending(
                    turn_id,
                    PendingToolCallBatch::new(
                        ToolCallBatchId::new(&format!("archive-batch-{index}"))
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
                            artifact_id(&format!("archive-result-{index}")),
                            ArtifactKind::Text,
                        ),
                    ),
                    ArtifactContent::text(format!(
                        "large tool result {index} {}",
                        "archive ballast ".repeat(4_000)
                    )),
                )
                .expect("tool result records");
        }
    }

    let current_sentinel = "archive-only current sentinel";
    let events = collect_step(&runtime, current_sentinel, StepContext::default()).await;

    assert_eq!(
        event_kind_names(&events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );
    assert!(
        compactor.recorded_requests().is_empty(),
        "archive-only compaction must not call the compaction model"
    );
    let requests = primary.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.continuations().len(),
        5,
        "all five tool turns must remain in the primary request"
    );
    let request_text = request
        .input()
        .iter()
        .map(|item| serde_json::to_string(item).expect("request item serializes"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(request_text.matches(current_sentinel).count(), 1);
    assert!(!request_text.contains("compacted-checkpoint:"));
    assert!(request.continuations().iter().any(|continuation| {
        continuation
            .result()
            .content()
            .as_str()
            .contains("\"merry_archived\":true")
    }));
    for index in 1..=5 {
        assert!(request_text.contains(&format!("archive-call-{index}")));
    }
}
