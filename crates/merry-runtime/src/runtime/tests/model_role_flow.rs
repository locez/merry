use super::*;

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
            "ArtifactRecorded",
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
            .any(|event| matches!(event.kind, RuntimeEventKind::StepCompleted)),
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
            .any(|event| matches!(event.kind, RuntimeEventKind::StepCompleted)),
        "tail seed step should complete"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_uses_context_compaction_role_when_configured() {
    let primary = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
    ]);
    let compactor =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text(
                    r#"{
                      "claims": [
                        {
                          "id": "c1",
                          "kind": "completed_action",
                          "text": "Old history was compacted.",
                          "refs": ["r1", "r2"]
                        }
                      ],
                      "working_intent": null
                    }"#,
                )],
                FinishReason::Stop,
            ),
        )])]);
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

    let outcome = runtime
        .compact_context_once(
            CitationCompactionPolicy::new(128, None, 4096, 2, 1200, 16).expect("valid policy"),
            StepContext::default(),
        )
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
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_accepts_streamed_text_delta_before_completed_response() {
    let primary = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
    ]);
    let candidate_json = r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "completed_action",
              "text": "Old history was compacted.",
              "refs": ["r1", "r2"]
            }
          ],
          "working_intent": null
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
            CitationCompactionPolicy::new(128, None, 4096, 2, 1200, 16).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("compaction accepts streamed text delta")
        .expect("compaction happened");

    assert_eq!(outcome.covered_history_item_count(), 2);
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
                ModelContent::text(&"a".repeat(260_000)).expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        Vec::new(),
        GenerationConfig::new(Some(10_000), false).expect("valid generation"),
        1,
    )
    .expect("valid request");

    let budget = request_context_budget(&capabilities, &request).expect("budget should calculate");

    assert_eq!(
        budget.window.source(),
        crate::ContextWindowSource::ProviderCapabilities
    );
    assert_eq!(budget.policy, ContextBudgetPolicy::Balanced);
    assert_eq!(budget.decision, CheckpointDecision::PlanCheckpoint);
    assert!(budget.dynamic_body_estimated_tokens >= budget.budget.soft_water_tokens());
    assert!(budget.dynamic_body_estimated_tokens < budget.budget.hard_water_tokens());
}
