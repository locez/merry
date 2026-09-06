use crate::judgment::{
    JudgmentContext, JudgmentError, JudgmentPurpose, JudgmentRecommendation, JudgmentSource,
    JudgmentSourceKind, NoopJudgmentSource, tests::memory_relevance_request,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn source_trait_can_be_called_through_arc_dyn() {
    let source: Arc<dyn JudgmentSource> = Arc::new(NoopJudgmentSource);

    let outcome = source
        .judge(
            memory_relevance_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect("noop source returns an advisory outcome");

    assert_eq!(outcome.purpose(), JudgmentPurpose::MemoryRelevance);
    assert_eq!(
        outcome.recommendation(),
        &JudgmentRecommendation::NoRecommendation
    );
    assert_eq!(outcome.confidence().as_f32(), 0.0);
}

#[tokio::test(flavor = "current_thread")]
async fn noop_source_returns_advisory_result_only() {
    let source = NoopJudgmentSource;

    let outcome = source
        .judge(
            memory_relevance_request(),
            JudgmentContext::new(CancellationToken::new()),
        )
        .await
        .expect("noop source returns an advisory outcome");

    assert_eq!(
        outcome.recommendation(),
        &JudgmentRecommendation::NoRecommendation
    );
    assert!(outcome.evidence().is_empty());
    assert_eq!(
        outcome.provenance().source_kind(),
        JudgmentSourceKind::Deterministic
    );
    assert_eq!(outcome.provenance().source_label(), "noop judgment source");
    assert!(outcome.rationale().contains("runtime policy"));
    assert_eq!(
        outcome.uncertainty(),
        "No semantic recommendation was produced."
    );
}

#[test]
fn cancellation_token_is_carried_in_context() {
    let token = CancellationToken::new();
    let context = JudgmentContext::new(token.clone());

    assert!(!context.cancellation_token().is_cancelled());
    token.cancel();
    assert!(context.cancellation_token().is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn noop_source_observes_pre_cancelled_context() {
    let token = CancellationToken::new();
    token.cancel();
    let source = NoopJudgmentSource;

    let error = source
        .judge(memory_relevance_request(), JudgmentContext::new(token))
        .await
        .expect_err("pre-cancelled context is rejected");

    assert_eq!(error, JudgmentError::Cancelled);
}
