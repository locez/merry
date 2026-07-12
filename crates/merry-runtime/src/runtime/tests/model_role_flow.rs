use super::*;
use merry_core::{PendingToolCallBatch, ToolCallBatchId};
use std::time::Duration;

#[path = "model_role_flow/compaction_generation.rs"]
mod compaction_generation;

#[test]
fn role_model_config_stores_all_roles_independently_and_overrides_same_role() {
    let first_primary_model = named_model("fake/primary-v1");
    let primary_model = named_model("fake/primary-v2");
    let first_tool_risk_model = named_model("fake/tool-risk-review-v1");
    let tool_risk_model = named_model("fake/tool-risk-review");
    let approval_model = named_model("fake/approval-review");
    let summary_model = named_model("fake/summary-memory");
    let compaction_model = named_model("fake/context-compaction");

    let runtime = Runtime::builder(session_id("runtime-role-model-config"))
        .model_provider(Arc::new(RecordingModelProvider::new()), first_primary_model)
        .model_provider(
            Arc::new(RecordingModelProvider::new()),
            primary_model.clone(),
        )
        .model_provider_for_role(
            RuntimeModelRole::ToolRiskReview,
            Arc::new(RecordingModelProvider::new()),
            first_tool_risk_model,
        )
        .model_provider_for_role(
            RuntimeModelRole::ApprovalReview,
            Arc::new(RecordingModelProvider::new()),
            approval_model.clone(),
        )
        .model_provider_for_role(
            RuntimeModelRole::SummaryMemory,
            Arc::new(RecordingModelProvider::new()),
            summary_model.clone(),
        )
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(RecordingModelProvider::new()),
            compaction_model.clone(),
        )
        .model_provider_for_role(
            RuntimeModelRole::ToolRiskReview,
            Arc::new(RecordingModelProvider::new()),
            tool_risk_model.clone(),
        )
        .build()
        .expect("runtime should build");

    for (role, expected_model) in [
        (RuntimeModelRole::Primary, &primary_model),
        (RuntimeModelRole::ToolRiskReview, &tool_risk_model),
        (RuntimeModelRole::ApprovalReview, &approval_model),
        (RuntimeModelRole::SummaryMemory, &summary_model),
        (RuntimeModelRole::ContextCompaction, &compaction_model),
    ] {
        assert_eq!(
            runtime.inner.model_configs.model_for_role(role),
            Some(expected_model)
        );
    }
}

#[test]
fn role_scoped_retry_policy_does_not_rewrite_existing_model_configs() {
    let initial_policy = ModelRetryPolicy::new(
        false,
        1,
        Duration::from_millis(1),
        Duration::from_millis(1),
        Duration::from_millis(1),
        false,
    )
    .expect("valid policy");
    let later_policy = ModelRetryPolicy::new(
        true,
        2,
        Duration::from_millis(1),
        Duration::from_millis(1),
        Duration::from_millis(1),
        false,
    )
    .expect("valid policy");

    let runtime = Runtime::builder(session_id("runtime-role-scoped-retry"))
        .model_retry_policy(initial_policy)
        .model_provider(
            Arc::new(RecordingModelProvider::new()),
            named_model("fake/primary"),
        )
        .model_provider_for_role_with_retry(
            RuntimeModelRole::ToolRiskReview,
            Arc::new(RecordingModelProvider::new()),
            named_model("fake/tool-risk"),
            later_policy,
        )
        .build()
        .expect("runtime should build");

    assert_eq!(
        runtime
            .inner
            .model_configs
            .get(RuntimeModelRole::Primary)
            .expect("primary config should exist")
            .retry_policy(),
        initial_policy
    );
    assert_eq!(
        runtime
            .inner
            .model_configs
            .get(RuntimeModelRole::ToolRiskReview)
            .expect("tool-risk config should exist")
            .retry_policy(),
        later_policy
    );
}

#[tokio::test(flavor = "current_thread")]
async fn step_uses_primary_model_and_does_not_call_any_non_primary_role_provider() {
    let primary = RecordingModelProvider::new();
    let tool_risk_review = RecordingModelProvider::new();
    let approval_review = RecordingModelProvider::new();
    let summary_memory = RecordingModelProvider::new();
    let context_compaction = RecordingModelProvider::new();
    let runtime = Runtime::builder(session_id("runtime-step-primary-role-model"))
        .model_provider(Arc::new(primary.clone()), named_model("fake/primary-step"))
        .model_provider_for_role(
            RuntimeModelRole::ToolRiskReview,
            Arc::new(tool_risk_review.clone()),
            named_model("fake/tool-risk-review-step"),
        )
        .model_provider_for_role(
            RuntimeModelRole::ApprovalReview,
            Arc::new(approval_review.clone()),
            named_model("fake/approval-review-step"),
        )
        .model_provider_for_role(
            RuntimeModelRole::SummaryMemory,
            Arc::new(summary_memory.clone()),
            named_model("fake/summary-memory-step"),
        )
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(context_compaction.clone()),
            named_model("fake/context-compaction-step"),
        )
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    let primary_requests = primary.recorded_requests();
    assert_eq!(primary.calls.load(Ordering::SeqCst), 1);
    assert_eq!(primary_requests.len(), 1);
    assert_eq!(
        primary_requests[0].model(),
        &named_model("fake/primary-step")
    );
    for provider in [
        &tool_risk_review,
        &approval_review,
        &summary_memory,
        &context_compaction,
    ] {
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(provider.recorded_requests().is_empty());
    }
}

async fn seed_two_history_items_for_compaction(runtime: &Runtime) {
    let events = collect_step(
        runtime,
        "old user message for compaction",
        crate::StepContext::default(),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "seed step should complete"
    );
    let events = collect_step(
        runtime,
        "retained tail user message",
        crate::StepContext::default(),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted)),
        "tail seed step should complete"
    );
}

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
        ModelCapabilities::new(true, true, false, true, Some(4_000), Some(512))
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
        ModelCapabilities::new(true, true, false, true, Some(4_000), Some(512))
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

#[test]
fn request_context_budget_uses_dynamic_estimate_watermarks() {
    let capabilities = ModelCapabilities::new(true, true, false, true, Some(100_000), Some(10_000))
        .expect("valid capabilities");
    let request = ModelRequest::new_with_continuations_and_stable_prefix(
        named_model("fake/budget-test"),
        vec![
            ModelMessage::new(
                ModelMessageRole::System,
                ModelContent::text("Base instructions.").expect("valid content"),
            )
            .expect("valid message"),
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text(&"a".repeat(320_000)).expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::new(Some(10_000), false).expect("valid generation"),
        1,
    )
    .expect("valid request");

    let budget =
        request_context_budget(&capabilities, &request, None).expect("budget should calculate");

    assert_eq!(
        budget.window.source(),
        crate::ContextWindowSource::ProviderCapabilities
    );
    assert_eq!(budget.policy, ContextBudgetPolicy::Balanced);
    assert_eq!(budget.decision, CheckpointDecision::PlanCheckpoint);
    assert!(budget.dynamic_body_estimated_tokens >= budget.budget.soft_water_tokens());
    assert!(budget.dynamic_body_estimated_tokens < budget.budget.hard_water_tokens());

    let usage = step_usage_context_snapshot(Some(&budget), true);
    assert_eq!(
        usage
            .compaction
            .expect("compaction usage should be available")
            .dynamic_body_estimated_tokens,
        Some(budget.dynamic_body_estimated_tokens)
    );
}

#[test]
fn request_context_budget_derives_default_output_reserve_from_window() {
    let request = ModelRequest::new_with_continuations_and_stable_prefix(
        named_model("fake/default-output-reserve"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("Need budget.").expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::default(),
        0,
    )
    .expect("valid request");

    for (window, expected_output_reserve) in [
        (32_000, 3_200),
        (64_000, 3_200),
        (128_000, 6_400),
        (256_000, 8_192),
        (512_000, 8_192),
        (1_000_000, 8_192),
        (2_000_000, 8_192),
    ] {
        let capabilities = ModelCapabilities::new(true, true, false, true, Some(window), None)
            .expect("valid capabilities");
        let budget =
            request_context_budget(&capabilities, &request, None).expect("budget should calculate");

        assert_eq!(
            budget.budget.output_reserve_tokens(),
            expected_output_reserve
        );
    }
}

#[test]
fn request_context_budget_uses_codex_style_fallback_for_unknown_models() {
    let capabilities =
        ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities");
    let request = ModelRequest::new_with_continuations_and_stable_prefix(
        named_model("unknown/model"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("Need budget.").expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::default(),
        0,
    )
    .expect("valid request");

    let budget =
        request_context_budget(&capabilities, &request, None).expect("budget should calculate");

    assert_eq!(budget.window.tokens(), 272_000);
    assert_eq!(budget.window.source(), crate::ContextWindowSource::Fallback);
    assert_eq!(budget.budget.effective_window_tokens(), 258_400);
}

#[test]
fn request_context_budget_prefers_an_explicit_window_override() {
    let capabilities = ModelCapabilities::new(true, true, false, true, Some(64_000), None)
        .expect("valid capabilities");
    let request = ModelRequest::new_with_continuations_and_stable_prefix(
        named_model("configured/model"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("Need budget.").expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::default(),
        0,
    )
    .expect("valid request");

    let budget = request_context_budget(&capabilities, &request, Some(128_000))
        .expect("budget should calculate");

    assert_eq!(budget.window.tokens(), 128_000);
    assert_eq!(
        budget.window.source(),
        crate::ContextWindowSource::ExplicitConfig
    );
}
