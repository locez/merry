#![cfg_attr(not(test), allow(dead_code))]

use super::{
    core::{
        JudgmentConfidence, JudgmentOutcome, JudgmentProvenance, JudgmentRecommendation,
        JudgmentRequest, JudgmentSourceKind,
    },
    error::JudgmentError,
};
use std::{future::Future, pin::Pin};
use tokio_util::sync::CancellationToken;

/// Result returned by a crate-internal advisory judgment source.
pub(crate) type JudgmentResult = Result<JudgmentOutcome, JudgmentError>;

/// Boxed judgment future used for object-safe async boundaries.
pub(crate) type JudgmentFuture<'a> = Pin<Box<dyn Future<Output = JudgmentResult> + Send + 'a>>;

/// Context passed to an advisory judgment source.
#[derive(Debug, Clone)]
pub(crate) struct JudgmentContext {
    cancellation_token: CancellationToken,
}

impl JudgmentContext {
    #[must_use]
    pub(crate) fn new(cancellation_token: CancellationToken) -> Self {
        Self { cancellation_token }
    }

    #[must_use]
    pub(crate) fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }
}

/// Object-safe crate-internal advisory judgment source boundary.
pub(crate) trait JudgmentSource: Send + Sync {
    fn judge<'a>(
        &'a self,
        request: JudgmentRequest,
        context: JudgmentContext,
    ) -> JudgmentFuture<'a>;
}

/// Deterministic placeholder source that produces no semantic recommendation.
#[derive(Debug, Default)]
pub(crate) struct NoopJudgmentSource;

impl JudgmentSource for NoopJudgmentSource {
    fn judge<'a>(
        &'a self,
        request: JudgmentRequest,
        context: JudgmentContext,
    ) -> JudgmentFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(JudgmentError::Cancelled);
            }

            JudgmentOutcome::new(
                request.purpose(),
                JudgmentRecommendation::NoRecommendation,
                JudgmentConfidence::new(0.0)?,
                Vec::new(),
                "No judgment source is configured; runtime policy must make any hard decision without this advisory signal.",
                "No semantic recommendation was produced.",
                JudgmentProvenance::new(JudgmentSourceKind::Deterministic, "noop judgment source")?,
            )
        })
    }
}
