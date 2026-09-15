//! Shared single-text model completion boundary.
//!
//! Permission review, tool-risk judgment, and context compaction all ask a
//! model for exactly one text answer and must treat stream surprises as typed
//! failures instead of guessing. This module owns that one contract so each
//! role does not re-derive it:
//!
//! - cooperative cancellation is classified once, from the caller token, the
//!   provider error kind, and the model's own finish reason;
//! - provider setup failures and stream failures stay distinguishable;
//! - tool calls, non-stop finishes, multi-output responses, and truncated
//!   streams are separate failures rather than one opaque "invalid response".
//!
//! The helper is policy-free: it does not retry, record events, or decide
//! whether a failure is acceptable. Callers map [`ModelCompletionError`] into
//! their own domain error and keep their own retry policy, and they own the
//! [`ModelStreamContext`] so provider-visible cache keys stay caller-controlled.

use futures_util::StreamExt;
use merry_llm::{
    FinishReason, ModelError, ModelEvent, ModelOutput, ModelProvider, ModelRequest,
    ModelStreamContext, ProviderErrorKind,
};
use tokio_util::sync::CancellationToken;

/// Returns whether a provider error must be treated as cooperative cancellation.
///
/// Both `ModelError::Cancelled` and a provider-reported cancellation carry
/// [`ProviderErrorKind::Cancelled`], so the normalized kind is sufficient.
pub(crate) fn is_cancelled_model_error(error: &ModelError) -> bool {
    error.kind() == ProviderErrorKind::Cancelled
}

/// Why one single-text completion did not produce an accepted answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelCompletionError {
    /// The caller token, the provider, or the model cancelled the request.
    Cancelled,
    /// The provider failed before the stream was established.
    Setup {
        /// Provider-neutral error category.
        kind: ProviderErrorKind,
        /// Actionable provider message.
        message: String,
    },
    /// The provider failed while the stream was being read.
    Stream {
        /// Provider-neutral error category.
        kind: ProviderErrorKind,
        /// Actionable provider message.
        message: String,
    },
    /// The model requested a tool call, which a single-text answer must not do.
    ToolCallRequested,
    /// The model stopped for a reason other than a normal stop.
    NonStopFinish {
        /// Finish reason reported by the provider.
        finish_reason: FinishReason,
    },
    /// The answer was not exactly one text item.
    NotSingleText,
    /// The stream ended before a completion event arrived.
    EndedBeforeCompletion,
}

/// Drives one provider stream and returns its single text answer.
///
/// The caller supplies the stream context so provider-visible cache keys and
/// continuation state stay caller-owned. Cancellation is checked before setup
/// and while reading, so a cancelled token never opens a provider request.
pub(crate) async fn complete_single_text(
    provider: &dyn ModelProvider,
    request: ModelRequest,
    stream_context: ModelStreamContext,
    token: &CancellationToken,
) -> Result<String, ModelCompletionError> {
    if token.is_cancelled() {
        return Err(ModelCompletionError::Cancelled);
    }
    let stream_result = tokio::select! {
        biased;
        () = token.cancelled() => return Err(ModelCompletionError::Cancelled),
        result = provider.stream_model(request, stream_context) => result,
    };
    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(error) if is_cancelled_model_error(&error) => {
            return Err(ModelCompletionError::Cancelled);
        }
        Err(error) => {
            return Err(ModelCompletionError::Setup {
                kind: error.kind(),
                message: error.message().to_owned(),
            });
        }
    };

    loop {
        let item = tokio::select! {
            biased;
            () = token.cancelled() => return Err(ModelCompletionError::Cancelled),
            item = stream.next() => item,
        };

        match item {
            Some(Ok(ModelEvent::Started | ModelEvent::OutputTextDelta { .. })) => {}
            Some(Ok(ModelEvent::ToolCallRequested { .. })) => {
                return Err(ModelCompletionError::ToolCallRequested);
            }
            Some(Ok(ModelEvent::Completed { response })) => {
                if response.finish_reason() == FinishReason::Cancelled {
                    return Err(ModelCompletionError::Cancelled);
                }
                if response.finish_reason() != FinishReason::Stop {
                    return Err(ModelCompletionError::NonStopFinish {
                        finish_reason: response.finish_reason(),
                    });
                }
                let [ModelOutput::Text { text }] = response.outputs() else {
                    return Err(ModelCompletionError::NotSingleText);
                };
                return Ok(text.clone());
            }
            Some(Err(error)) => {
                if is_cancelled_model_error(&error) {
                    return Err(ModelCompletionError::Cancelled);
                }
                return Err(ModelCompletionError::Stream {
                    kind: error.kind(),
                    message: error.message().to_owned(),
                });
            }
            None => return Err(ModelCompletionError::EndedBeforeCompletion),
        }
    }
}
