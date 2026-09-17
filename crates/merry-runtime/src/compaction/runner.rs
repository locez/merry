use super::{CitationCompactionInput, checkpoint_from_candidate_json};
use crate::{
    RuntimeError,
    model_completion::{ModelCompletionError, complete_single_text},
    token_estimate::estimate_model_input_tokens,
};
use merry_core::{ProviderName, SessionId};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelProvider, ModelRequest, ModelStreamContext,
    ProviderErrorKind,
};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const MAX_COMPACTION_PROVIDER_ATTEMPTS: usize = 2;
const COMPACTION_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Resolves the total token window one compaction request may occupy.
///
/// The compaction model may be a different provider than the primary model. A
/// provider that reports no input window keeps the primary window as the
/// conservative assumption; a provider that reports a smaller window than the
/// primary context is a configuration error rather than a request to shrink.
pub(crate) fn compaction_model_window(
    capabilities: &ModelCapabilities,
    primary_window_tokens: u64,
    session_id: &SessionId,
    provider_name: &ProviderName,
) -> Result<u64, RuntimeError> {
    match capabilities.max_input_tokens() {
        Some(compactor_window_tokens) if compactor_window_tokens < primary_window_tokens => {
            Err(RuntimeError::CompactionModelWindowTooSmall {
                primary_window_tokens,
                compactor_window_tokens,
            })
        }
        Some(compactor_window_tokens) => Ok(compactor_window_tokens),
        None => {
            tracing::debug!(
                event = "runtime.compaction.model_window_assumed",
                session_id = session_id.as_str(),
                provider = provider_name.as_str(),
                primary_window_tokens,
                "compaction model input capability is absent; assuming the primary context window"
            );
            Ok(primary_window_tokens)
        }
    }
}

/// Returns the input and output tokens one compaction request needs from a window.
///
/// Compaction reasoning and checkpoint text share the provider output ceiling,
/// so callers must size the window against both numbers together.
pub(crate) fn compaction_request_required_tokens(request: &ModelRequest) -> (u64, u64) {
    (
        estimate_model_input_tokens(request.input()),
        request.generation().max_output_tokens().unwrap_or(0),
    )
}

/// Checks that one compaction request fits an already resolved compaction window.
///
/// The caller resolves the window once per attempt through
/// [`compaction_model_window`], so this stays a pure check over the request and
/// does not repeat provider-window discovery or its diagnostics.
pub(crate) fn validate_compaction_model_window(
    request: &ModelRequest,
    compactor_window_tokens: u64,
) -> Result<(), RuntimeError> {
    // Compaction reasoning and checkpoint text share the provider output
    // ceiling, so input and output must fit the window together. A request that
    // only fits because its output budget is ignored would be truncated by the
    // provider, which is exactly what this check exists to prevent.
    let (estimated_input_tokens, max_output_tokens) = compaction_request_required_tokens(request);
    let required_tokens = estimated_input_tokens
        .checked_add(max_output_tokens)
        .ok_or(RuntimeError::CompactionModelRequestTooLarge {
            estimated_input_tokens,
            max_output_tokens,
            compactor_window_tokens,
        })?;
    if required_tokens > compactor_window_tokens {
        return Err(RuntimeError::CompactionModelRequestTooLarge {
            estimated_input_tokens,
            max_output_tokens,
            compactor_window_tokens,
        });
    }
    Ok(())
}

pub(crate) async fn generate_validated_compaction_candidate(
    provider: Arc<dyn ModelProvider>,
    request: ModelRequest,
    stream_context: ModelStreamContext,
    input: &CitationCompactionInput,
    token: &CancellationToken,
) -> Result<String, RuntimeError> {
    for attempt in 1..=MAX_COMPACTION_PROVIDER_ATTEMPTS {
        if token.is_cancelled() {
            return Err(cancelled_setup_error("before compaction model setup"));
        }
        tracing::debug!(
            event = "runtime.compaction.attempt_started",
            attempt,
            max_attempts = MAX_COMPACTION_PROVIDER_ATTEMPTS,
            model = request.model().as_str(),
            message_count = request.messages().len(),
            estimated_input_tokens = estimate_model_input_tokens(request.input()),
            "starting compaction model attempt"
        );
        let candidate = match run_compaction_attempt(
            provider.as_ref(),
            request.clone(),
            stream_context.clone(),
            token,
        )
        .await
        {
            Ok(candidate) => candidate,
            Err(failure) => {
                trace_attempt_failure(attempt, &failure);
                if failure.cancelled
                    || !failure.retryable
                    || attempt == MAX_COMPACTION_PROVIDER_ATTEMPTS
                {
                    return Err(failure.error);
                }
                trace_retry(attempt, &failure.error);
                wait_before_compaction_retry(token).await?;
                continue;
            }
        };

        match checkpoint_from_candidate_json(
            input.manifest().checkpoint_id().clone(),
            input,
            &candidate,
        ) {
            Ok(_) => return Ok(candidate),
            Err(error) if attempt < MAX_COMPACTION_PROVIDER_ATTEMPTS => {
                trace_retry(attempt, &error);
                wait_before_compaction_retry(token).await?;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("compaction attempt loop always returns on its final attempt")
}

async fn run_compaction_attempt(
    provider: &dyn ModelProvider,
    request: ModelRequest,
    stream_context: ModelStreamContext,
    token: &CancellationToken,
) -> Result<String, AttemptFailure> {
    complete_single_text(provider, request, stream_context, token)
        .await
        .map_err(|error| completion_failure(&error))
}

fn completion_failure(error: &ModelCompletionError) -> AttemptFailure {
    match error {
        ModelCompletionError::Cancelled => {
            AttemptFailure::cancelled(cancelled_stream_error("while running the compaction model"))
        }
        ModelCompletionError::Setup { kind, message } => AttemptFailure {
            error: RuntimeError::CompactionModelSetup {
                message: message.clone(),
            },
            cancelled: false,
            retryable: is_retryable_provider_error(*kind),
        },
        ModelCompletionError::Stream { kind, message } => AttemptFailure {
            error: RuntimeError::CompactionModelStream {
                message: message.clone(),
            },
            cancelled: false,
            retryable: is_retryable_provider_error(*kind),
        },
        ModelCompletionError::ToolCallRequested => {
            AttemptFailure::retryable(RuntimeError::CompactionModelStream {
                message: "compaction model requested a tool call".to_owned(),
            })
        }
        ModelCompletionError::NonStopFinish {
            finish_reason,
            finish_detail,
        } => {
            let detail = finish_detail
                .map(|detail| format!(" ({})", detail.as_str()))
                .unwrap_or_default();
            let message = format!("compaction model finished with {finish_reason:?}{detail}");
            if is_truncated_finish(*finish_reason) {
                // Re-running the identical request cannot fix an exhausted
                // output budget; the caller owns the degraded re-plan.
                AttemptFailure {
                    error: RuntimeError::CompactionModelTruncated { message },
                    cancelled: false,
                    retryable: false,
                }
            } else {
                AttemptFailure::retryable(RuntimeError::CompactionModelStream { message })
            }
        }
        ModelCompletionError::NotSingleText => {
            AttemptFailure::retryable(RuntimeError::CompactionModelStream {
                message: "compaction model must return exactly one text output".to_owned(),
            })
        }
        ModelCompletionError::EndedBeforeCompletion => {
            AttemptFailure::retryable(RuntimeError::CompactionModelStream {
                message: "compaction model stream ended before completion".to_owned(),
            })
        }
    }
}

/// Returns whether one compaction attempt may be retried for this provider error.
///
/// This is the runtime's attempt-level policy, not the provider-level
/// [`merry_llm::ModelRetryPolicy`]: provider retries already ran inside
/// `stream_model`, so an error that reaches runtime is what the whole attempt
/// observed. Request-shape, authentication, and cancellation failures cannot be
/// fixed by another attempt, so only transport and availability classes retry.
fn is_retryable_provider_error(kind: ProviderErrorKind) -> bool {
    !matches!(
        kind,
        ProviderErrorKind::InvalidRequest
            | ProviderErrorKind::InvalidToolCall
            | ProviderErrorKind::Cancelled
            | ProviderErrorKind::Authentication
    )
}

fn cancelled_setup_error(stage: &'static str) -> RuntimeError {
    RuntimeError::CompactionModelSetup {
        message: format!("compaction cancelled {stage}"),
    }
}

/// Returns whether one finish means the provider cut the checkpoint short.
///
/// A length stop means the model exhausted its output budget, including
/// reasoning tokens, before the candidate was complete. A blocked response is
/// not a truncation: a smaller window would be filtered again.
fn is_truncated_finish(finish_reason: FinishReason) -> bool {
    matches!(finish_reason, FinishReason::Length)
}

fn cancelled_stream_error(stage: &'static str) -> RuntimeError {
    RuntimeError::CompactionModelStream {
        message: format!("compaction cancelled {stage}"),
    }
}

async fn wait_before_compaction_retry(token: &CancellationToken) -> Result<(), RuntimeError> {
    tokio::select! {
        biased;
        () = token.cancelled() => Err(cancelled_setup_error("before compaction retry")),
        () = tokio::time::sleep(COMPACTION_RETRY_DELAY) => Ok(()),
    }
}

fn trace_retry(attempt: usize, error: &RuntimeError) {
    tracing::debug!(
        event = "runtime.compaction.retry",
        attempt,
        next_attempt = attempt + 1,
        max_attempts = MAX_COMPACTION_PROVIDER_ATTEMPTS,
        error = %error,
        "retrying failed compaction model attempt"
    );
}

fn trace_attempt_failure(attempt: usize, failure: &AttemptFailure) {
    tracing::debug!(
        event = "runtime.compaction.attempt_failed",
        attempt,
        max_attempts = MAX_COMPACTION_PROVIDER_ATTEMPTS,
        cancelled = failure.cancelled,
        retryable = failure.retryable,
        error = %failure.error,
        "compaction model attempt failed"
    );
}

struct AttemptFailure {
    error: RuntimeError,
    cancelled: bool,
    retryable: bool,
}

impl AttemptFailure {
    fn retryable(error: RuntimeError) -> Self {
        Self {
            error,
            cancelled: false,
            retryable: true,
        }
    }

    fn cancelled(error: RuntimeError) -> Self {
        Self {
            error,
            cancelled: true,
            retryable: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_llm::{FinishDetail, FinishReason};

    #[test]
    fn non_stop_finish_message_carries_the_provider_detail() {
        let failure = completion_failure(&ModelCompletionError::NonStopFinish {
            finish_reason: FinishReason::Length,
            finish_detail: Some(FinishDetail::MaxOutputTokens),
        });

        assert!(
            !failure.retryable,
            "a truncated checkpoint must not be retried with the identical request"
        );
        assert!(
            failure
                .error
                .to_string()
                .contains("compaction model finished with Length (max_output_tokens)"),
            "unexpected message: {}",
            failure.error
        );
        assert!(matches!(
            failure.error,
            RuntimeError::CompactionModelTruncated { .. }
        ));
    }

    #[test]
    fn non_stop_finish_without_detail_keeps_the_plain_reason() {
        let failure = completion_failure(&ModelCompletionError::NonStopFinish {
            finish_reason: FinishReason::Blocked,
            finish_detail: None,
        });

        assert!(
            failure
                .error
                .to_string()
                .contains("compaction model finished with Blocked"),
            "unexpected message: {}",
            failure.error
        );
    }
}
