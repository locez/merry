//! Model reviewer contract tests owned by `permission::review`.

use super::call;
use crate::model_config::ModelProviderConfig;
use crate::permission::review::{
    PERMISSION_REVIEW_SCHEMA_VERSION, parse_permission_review_model_output,
};
use crate::permission::{
    ModelBackedPermissionAdmissionSource, PermissionAdmissionContext, PermissionAdmissionError,
    PermissionAdmissionSource, PermissionRequest, PermissionReviewRisk,
    PermissionUserAuthorization, permission_request_from_call,
};
use merry_llm::{
    FinishReason, ModelEvent, ModelName, ModelOutput, ModelResponse, ModelRetryPolicy,
    ReasoningEffort, testing::FakeModelProvider,
};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Builds a reviewer source backed by one scripted provider response.
fn review_source(provider: Arc<FakeModelProvider>) -> ModelBackedPermissionAdmissionSource {
    ModelBackedPermissionAdmissionSource::from_config(ModelProviderConfig::new(
        provider,
        ModelName::new("fake/reviewer").expect("model name should be valid"),
        ModelRetryPolicy::default(),
    ))
    .expect("review source should build")
}

/// Builds the permission request every reviewer test reviews.
fn review_request() -> PermissionRequest {
    permission_request_from_call(
        &call(json!({
            "reason": "Confirm the endpoint is reachable",
            "requested": { "network": true },
            "for_action": { "command": "curl -sI https://example.com", "cwd": null }
        })),
        Vec::new(),
    )
    .expect("request should parse")
}

#[test]
fn model_review_parser_maps_approve_and_deny() {
    let approved = parse_permission_review_model_output(
            r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"high","rationale":"Task explicitly asks for it."}"#,
        )
        .expect("approve parses");
    assert!(approved.is_approved());

    let denied = parse_permission_review_model_output(
            r#"{"schema_version":"permission_review.v1","decision":"deny","risk":"high","user_authorization":"unknown","rationale":"No user authorization."}"#,
        )
        .expect("deny parses");
    assert!(!denied.is_approved());
}

#[test]
fn model_review_does_not_auto_approve_inconsistent_risk_or_authorization() {
    let decision = parse_permission_review_model_output(
            r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"high","user_authorization":"unknown","rationale":"The command may be useful."}"#,
        )
        .expect("inconsistent approval should become a structured denial");

    assert!(!decision.is_approved());
    assert!(
        decision
            .review()
            .rationale()
            .contains("not internally consistent")
    );
}

#[test]
fn model_review_parser_ignores_reviewer_extra_fields() {
    // Reviewers on smaller models may echo prompt metadata such as
    // `reviewed_tool_call_id` or append their own commentary fields. Those
    // fields carry no authority, so they must not fail an otherwise valid
    // review.
    let decision = parse_permission_review_model_output(
        r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"high","rationale":"The exact command is grounded in the user's task.","reviewed_tool_call_id":"call-permission","reviewed_tool_name":"request_permissions","review_only":false,"confidence":0.8}"#,
    )
    .expect("extra reviewer fields should be ignored");

    assert!(decision.is_approved());
    assert_eq!(decision.review().risk(), PermissionReviewRisk::Low);
    assert_eq!(
        decision.review().user_authorization(),
        PermissionUserAuthorization::High
    );
    assert_eq!(
        decision.review().rationale(),
        "The exact command is grounded in the user's task."
    );
}

#[test]
fn model_review_parser_rejects_contract_violations() {
    let missing_field = r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"high"}"#;
    assert!(parse_permission_review_model_output(missing_field).is_err());

    for invalid in [
        r#"{"schema_version":"permission_review.v2","decision":"approve","risk":"low","user_authorization":"high","rationale":"stale schema"}"#,
        r#"{"schema_version":"permission_review.v1","decision":"maybe","risk":"low","user_authorization":"high","rationale":"unsupported decision"}"#,
        r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"extreme","user_authorization":"high","rationale":"unsupported risk"}"#,
        r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"certain","rationale":"unsupported authorization"}"#,
        r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"high","rationale":"   "}"#,
    ] {
        let error = parse_permission_review_model_output(invalid)
            .expect_err("contract violations must stay rejected");
        assert!(matches!(
            error,
            PermissionAdmissionError::InvalidReviewOutput { .. }
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn model_review_request_declares_the_accepted_schema_version() {
    // The reviewer can only answer in the schema it was told about, so the
    // prompt and the parser must agree on one schema version constant.
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(
                r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"high","rationale":"The exact command is grounded in the task."}"#,
            )],
            FinishReason::Stop,
            None,
        ),
    })]));
    let source = review_source(provider.clone());

    source
        .review(
            review_request(),
            PermissionAdmissionContext::new(CancellationToken::new()),
        )
        .await
        .expect("review should be accepted");

    let recorded = provider.recorded_requests();
    let [model_request] = recorded.as_slice() else {
        panic!("expected exactly one recorded review request");
    };
    assert_eq!(model_request.messages().len(), 2);
    let system = model_request.messages()[0].content().as_text();
    let user = model_request.messages()[1].content().as_text();
    assert!(system.contains(PERMISSION_REVIEW_SCHEMA_VERSION));
    assert!(system.contains("Return only those five fields"));
    assert!(
        user.contains(&format!(
            "schema_version={}\n",
            PERMISSION_REVIEW_SCHEMA_VERSION
        )),
        "user prompt must declare the schema version the parser accepts"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_review_request_keeps_thinking_modest_and_output_budget_wide() {
    // Reviewers on reasoning models bill hidden reasoning tokens against the
    // same ceiling as the answer. The reviewer request must therefore keep
    // thinking at a low effort and leave the ceiling far above one review
    // JSON, otherwise an ordinary reasoning pass is truncated before the
    // reviewer can answer in the accepted schema.
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(
                r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"high","rationale":"The exact command is grounded in the task."}"#,
            )],
            FinishReason::Stop,
            None,
        ),
    })]));
    let source = review_source(provider.clone());

    source
        .review(
            review_request(),
            PermissionAdmissionContext::new(CancellationToken::new()),
        )
        .await
        .expect("review should be accepted");

    let recorded = provider.recorded_requests();
    let [model_request] = recorded.as_slice() else {
        panic!("expected exactly one recorded review request");
    };
    let generation = model_request.generation();
    assert_eq!(generation.max_output_tokens(), Some(2048));
    assert_eq!(
        generation.reasoning_effort().map(ReasoningEffort::as_str),
        Some("low")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_review_reports_a_truncated_answer_as_an_output_budget_failure() {
    // A reviewer that runs out of output tokens never reaches the review
    // schema. The failure must name the output budget rather than read as a
    // reviewer that returned output outside the contract.
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(
                r#"{"schema_version":"permission_review.v1","decision":"approve","#,
            )],
            FinishReason::Length,
            None,
        ),
    })]));
    let source = review_source(provider);

    let error = source
        .review(
            review_request(),
            PermissionAdmissionContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("a truncated review must not be accepted");

    let message = error.to_string();
    assert!(
        matches!(
            &error,
            PermissionAdmissionError::ReviewOutputTruncated { finish_reason }
                if *finish_reason == FinishReason::Length
        ),
        "message was {message}"
    );
    assert!(
        message.contains("ran out of output tokens"),
        "message should name the exhausted output budget: {message}"
    );
    assert!(message.contains("Length"), "message was {message}");
}

#[tokio::test(flavor = "current_thread")]
async fn model_review_separates_provider_failures_from_invalid_reviewer_output() {
    // A reviewer the provider never let answer is a failed review, not a
    // contract violation. Keeping the two apart lets runtime policy retry or
    // escalate instead of recording the reviewer as non-compliant.
    for (finish_reason, expected) in [
        (FinishReason::Blocked, "safety filter"),
        (FinishReason::Error, "failed permission review response"),
    ] {
        let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("no review decision")],
                finish_reason,
                None,
            ),
        })]));
        let source = review_source(provider);

        let error = source
            .review(
                review_request(),
                PermissionAdmissionContext::new(CancellationToken::new()),
            )
            .await
            .expect_err("a non-stop review must not be accepted");

        assert!(
            matches!(&error, PermissionAdmissionError::ReviewFailed { .. }),
            "{finish_reason:?} should stay a failed review, got {error}"
        );
        assert!(
            error.to_string().contains(expected),
            "message was {error}, expected {expected}"
        );
    }
}
