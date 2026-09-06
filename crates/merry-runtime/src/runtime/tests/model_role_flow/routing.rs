use crate::{
    RuntimeModelRole,
    runtime::{
        Runtime,
        tests::support::{
            common::{collect_step, event_kind_names, named_model, session_id},
            model_provider::RecordingModelProvider,
        },
    },
};
use merry_llm::ModelRetryPolicy;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

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
