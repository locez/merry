use crate::judgment::{
    JudgmentContext, JudgmentError, JudgmentEvidence, JudgmentPurpose, JudgmentRecommendation,
    JudgmentRiskLevel, JudgmentSource, JudgmentSourceKind, MODEL_BACKED_JUDGMENT_MAX_OUTPUT_TOKENS,
    MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION, ModelBackedJudgmentSource,
    tests::{
        SetupErrorModelProvider, artifact_id, completed_outputs_event, evidence,
        memory_relevance_request, model_backed_source, model_name, model_tool_call,
        model_tool_risk_output, tool_risk_request, tool_risk_request_with_evidence,
    },
};
use merry_core::{EvidenceLocator, EvidenceRef};
use merry_llm::{
    FinishReason, ModelError, ModelEvent, ModelMessageRole, ModelOutput, ProviderErrorKind,
    testing::FakeModelProvider,
};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_happy_path_returns_tool_risk_with_llm_provenance_and_evidence() {
    let first = evidence("tool call", "tool-call");
    let second = evidence("policy note", "policy-note");
    let request = tool_risk_request_with_evidence(vec![first.clone(), second.clone()]);
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text(&model_tool_risk_output(
            "high",
            vec![json!({ "index": 1, "label": "policy note" })],
        ))],
        FinishReason::Stop,
    ))]);
    let source = model_backed_source(provider.clone());

    let outcome = source
        .judge(request, JudgmentContext::new(CancellationToken::new()))
        .await
        .expect("valid model-backed judgment returns an outcome");

    assert_eq!(outcome.purpose(), JudgmentPurpose::ToolRiskReview);
    assert_eq!(
        outcome.recommendation(),
        &JudgmentRecommendation::ToolRiskReview {
            risk: JudgmentRiskLevel::High,
            concerns: vec!["The pending tool path may affect external state.".to_owned()],
        }
    );
    assert_eq!(outcome.evidence(), &[second]);
    assert_eq!(outcome.provenance().source_kind(), JudgmentSourceKind::Llm);
    assert_eq!(
        outcome.provenance().source_label(),
        "test model judgment source"
    );
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_records_expected_model_request_shape() {
    let request = tool_risk_request_with_evidence(vec![
        JudgmentEvidence::new(
            "tool call",
            EvidenceRef::new(
                artifact_id("tool-call"),
                EvidenceLocator::line_range(3, 9).expect("valid line range"),
            ),
        )
        .expect("judgment evidence is valid"),
        JudgmentEvidence::new(
            "policy note",
            EvidenceRef::new(
                artifact_id("policy-note"),
                EvidenceLocator::json_pointer("/risk").expect("valid json pointer"),
            ),
        )
        .expect("judgment evidence is valid"),
    ]);
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text(&model_tool_risk_output(
            "low",
            vec![json!({ "index": 0, "label": "tool call" })],
        ))],
        FinishReason::Stop,
    ))]);
    let source = model_backed_source(provider.clone());

    source
        .judge(request, JudgmentContext::new(CancellationToken::new()))
        .await
        .expect("valid model-backed judgment returns an outcome");

    let recorded = provider.recorded_requests();
    let [model_request] = recorded.as_slice() else {
        panic!("expected exactly one recorded model request");
    };
    assert_eq!(model_request.model(), &model_name());
    assert_eq!(model_request.messages().len(), 2);
    assert_eq!(model_request.messages()[0].role(), ModelMessageRole::System);
    assert_eq!(model_request.messages()[1].role(), ModelMessageRole::User);
    assert!(model_request.tools().is_empty());
    assert!(model_request.continuations().is_empty());
    assert_eq!(
        model_request.generation().max_output_tokens(),
        Some(MODEL_BACKED_JUDGMENT_MAX_OUTPUT_TOKENS)
    );
    assert!(!model_request.generation().allow_parallel_tool_calls());

    let system = model_request.messages()[0].content().as_text();
    let user = model_request.messages()[1].content().as_text();
    assert!(system.contains(MODEL_JUDGMENT_OUTPUT_SCHEMA_VERSION));
    assert!(system.contains("purpose tool_risk_review"));
    assert!(system.contains("Return exactly one JSON object"));
    assert!(user.contains("schema_version=merry.model_judgment_output.v1\n"));
    assert!(user.contains("purpose=tool_risk_review\n"));
    assert!(user.contains("subject=lookup tool call\n"));
    assert!(user.contains("input=Review whether the pending tool request has semantic risk.\n"));
    assert!(user.contains("constraints.0=advisory semantic signal only\n"));
    assert!(user.contains("evidence.0.label=tool call\n"));
    assert!(user.contains("evidence.0.artifact_id=tool-call\n"));
    assert!(user.contains("evidence.0.locator=line:3-9\n"));
    assert!(user.contains("evidence.1.label=policy note\n"));
    assert!(user.contains("evidence.1.artifact_id=policy-note\n"));
    assert!(user.contains("evidence.1.locator=json:/risk\n"));
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_rejects_non_tool_risk_before_provider_call() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text(&model_tool_risk_output(
            "low",
            Vec::new(),
        ))],
        FinishReason::Stop,
    ))]);
    let source = model_backed_source(provider.clone());

    let error = source
        .judge(
            memory_relevance_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("non-tool-risk request rejects");

    assert_eq!(
        error,
        JudgmentError::ModelJudgmentPurposeRequired {
            actual_purpose: JudgmentPurpose::MemoryRelevance,
        }
    );
    assert!(provider.recorded_requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_pre_cancelled_context_records_no_provider_request() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text(&model_tool_risk_output(
            "low",
            Vec::new(),
        ))],
        FinishReason::Stop,
    ))]);
    let source = model_backed_source(provider.clone());
    let token = CancellationToken::new();
    token.cancel();

    let error = source
        .judge(tool_risk_request(), JudgmentContext::new(token))
        .await
        .expect_err("pre-cancelled context rejects");

    assert_eq!(error, JudgmentError::Cancelled);
    assert!(provider.recorded_requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_stream_cancellation_maps_to_cancelled() {
    let provider = FakeModelProvider::new(vec![Err(ModelError::Cancelled)]);
    let source = model_backed_source(provider.clone());

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("stream cancellation rejects");

    assert_eq!(error, JudgmentError::Cancelled);
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_provider_cancelled_kind_maps_to_cancelled() {
    let provider = FakeModelProvider::new(vec![Err(ModelError::provider(
        ProviderErrorKind::Cancelled,
        "provider cancelled request",
    ))]);
    let source = model_backed_source(provider);

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("provider cancellation rejects");

    assert_eq!(error, JudgmentError::Cancelled);
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_provider_setup_error_maps_to_typed_cloneable_error() {
    let source = ModelBackedJudgmentSource::new(
        Arc::new(SetupErrorModelProvider::new(
            ProviderErrorKind::Authentication,
            "provider credentials are unavailable",
        )),
        model_name(),
        "test model judgment source",
    )
    .expect("model-backed judgment source is valid");

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("provider setup error rejects");

    assert_eq!(
        error.clone(),
        JudgmentError::ModelJudgmentProviderSetup {
            kind: ProviderErrorKind::Authentication,
            message: "provider credentials are unavailable".to_owned(),
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_stream_error_maps_to_typed_cloneable_error() {
    let provider = FakeModelProvider::new(vec![Err(ModelError::provider(
        ProviderErrorKind::Unavailable,
        "provider stream failed",
    ))]);
    let source = model_backed_source(provider);

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("stream provider error rejects");

    assert_eq!(
        error.clone(),
        JudgmentError::ModelJudgmentProviderStream {
            kind: ProviderErrorKind::Unavailable,
            message: "provider stream failed".to_owned(),
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_invalid_response_shapes_reject() {
    for (script, expected_reason) in [
        (
            Vec::new(),
            "model judgment stream ended before completed event",
        ),
        (
            vec![Ok(ModelEvent::ToolCallRequested {
                call: model_tool_call(),
            })],
            "model judgment stream must not request tools",
        ),
        (
            vec![Ok(completed_outputs_event(
                vec![ModelOutput::text(&model_tool_risk_output(
                    "low",
                    Vec::new(),
                ))],
                FinishReason::Length,
            ))],
            "model judgment completed without stop finish reason",
        ),
        (
            vec![Ok(completed_outputs_event(Vec::new(), FinishReason::Stop))],
            "model judgment stop output must contain exactly one text item",
        ),
        (
            vec![Ok(completed_outputs_event(
                vec![
                    ModelOutput::text(&model_tool_risk_output("low", Vec::new())),
                    ModelOutput::text(&model_tool_risk_output("medium", Vec::new())),
                ],
                FinishReason::Stop,
            ))],
            "model judgment stop output must contain exactly one text item",
        ),
        (
            vec![Ok(completed_outputs_event(
                vec![ModelOutput::tool_call(model_tool_call())],
                FinishReason::Stop,
            ))],
            "model judgment stop output must contain exactly one text item",
        ),
        (
            vec![Ok(completed_outputs_event(
                vec![
                    ModelOutput::text(&model_tool_risk_output("low", Vec::new())),
                    ModelOutput::tool_call(model_tool_call()),
                ],
                FinishReason::Stop,
            ))],
            "model judgment stop output must contain exactly one text item",
        ),
    ] {
        let provider = FakeModelProvider::new(script);
        let source = model_backed_source(provider);

        let error = source
            .judge(
                tool_risk_request(),
                JudgmentContext::new(CancellationToken::new()),
            )
            .await
            .expect_err("invalid model response shape rejects");

        assert_eq!(
            error,
            JudgmentError::InvalidModelJudgmentResponseShape {
                reason: expected_reason,
            }
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_rejects_explicit_non_stop_finish_reasons() {
    for finish_reason in [FinishReason::ToolCalls, FinishReason::Error] {
        let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
            vec![ModelOutput::text(&model_tool_risk_output(
                "low",
                Vec::new(),
            ))],
            finish_reason,
        ))]);
        let source = model_backed_source(provider);

        let error = source
            .judge(
                tool_risk_request(),
                JudgmentContext::new(CancellationToken::new()),
            )
            .await
            .expect_err("non-stop finish reason rejects");

        assert_eq!(
            error,
            JudgmentError::InvalidModelJudgmentResponseShape {
                reason: "model judgment completed without stop finish reason",
            }
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_completed_cancelled_finish_maps_to_cancelled() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        Vec::new(),
        FinishReason::Cancelled,
    ))]);
    let source = model_backed_source(provider);

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("cancelled finish rejects");

    assert_eq!(error, JudgmentError::Cancelled);
}

#[tokio::test(flavor = "current_thread")]
async fn model_backed_judgment_invalid_strict_json_propagates_parser_error() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text("not json")],
        FinishReason::Stop,
    ))]);
    let source = model_backed_source(provider);

    let error = source
        .judge(
            tool_risk_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("invalid strict JSON rejects");

    assert_eq!(error, JudgmentError::InvalidModelJudgmentOutput);
}
