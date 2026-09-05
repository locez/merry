use super::{CitationCompactionInput, checkpoint_from_candidate_json};
use crate::{RuntimeError, token_estimate::estimate_model_input_tokens};
use futures_util::StreamExt;
use merry_core::{ProviderName, SessionId};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelOutput, ModelProvider,
    ModelRequest, ModelStreamContext, ProviderErrorKind,
};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const MAX_COMPACTION_PROVIDER_ATTEMPTS: usize = 2;
const COMPACTION_RETRY_DELAY: Duration = Duration::from_millis(100);

pub(crate) fn validate_compaction_model_window(
    capabilities: &ModelCapabilities,
    request: &ModelRequest,
    primary_window_tokens: u64,
    session_id: &SessionId,
    provider_name: &ProviderName,
) -> Result<(), RuntimeError> {
    let compactor_window_tokens = match capabilities.max_input_tokens() {
        Some(compactor_window_tokens) if compactor_window_tokens < primary_window_tokens => {
            return Err(RuntimeError::CompactionModelWindowTooSmall {
                primary_window_tokens,
                compactor_window_tokens,
            });
        }
        Some(compactor_window_tokens) => compactor_window_tokens,
        None => {
            tracing::debug!(
                event = "runtime.compaction.model_window_assumed",
                session_id = session_id.as_str(),
                provider = provider_name.as_str(),
                primary_window_tokens,
                "compaction model input capability is absent; assuming the primary context window"
            );
            primary_window_tokens
        }
    };
    let estimated_input_tokens = estimate_model_input_tokens(request.input());
    if estimated_input_tokens > compactor_window_tokens {
        return Err(RuntimeError::CompactionModelInputTooLarge {
            estimated_input_tokens,
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
    let setup = provider.stream_model(request, stream_context);
    tokio::pin!(setup);
    let mut stream = tokio::select! {
        biased;
        () = token.cancelled() => {
            return Err(AttemptFailure::cancelled(cancelled_setup_error(
                "during compaction model setup",
            )));
        }
        result = &mut setup => match result {
            Ok(stream) => stream,
            Err(error) => return Err(setup_failure(error)),
        },
    };

    loop {
        let item = tokio::select! {
            biased;
            () = token.cancelled() => {
                return Err(AttemptFailure::cancelled(cancelled_stream_error(
                    "while reading compaction model stream",
                )));
            }
            item = stream.next() => item,
        };
        match item {
            Some(Ok(ModelEvent::Started | ModelEvent::OutputTextDelta { .. })) => {}
            Some(Ok(ModelEvent::ToolCallRequested { .. })) => {
                return Err(AttemptFailure::retryable(
                    RuntimeError::CompactionModelStream {
                        message: "compaction model requested a tool call".to_owned(),
                    },
                ));
            }
            Some(Ok(ModelEvent::Completed { response })) => {
                if response.finish_reason() == FinishReason::Cancelled {
                    return Err(AttemptFailure::cancelled(cancelled_stream_error(
                        "because the compaction model reported a cancelled finish",
                    )));
                }
                if response.finish_reason() != FinishReason::Stop {
                    return Err(AttemptFailure::retryable(
                        RuntimeError::CompactionModelStream {
                            message: format!(
                                "compaction model finished with {:?}",
                                response.finish_reason()
                            ),
                        },
                    ));
                }
                let [ModelOutput::Text { text }] = response.outputs() else {
                    return Err(AttemptFailure::retryable(
                        RuntimeError::CompactionModelStream {
                            message: "compaction model must return exactly one text output"
                                .to_owned(),
                        },
                    ));
                };
                return Ok(text.clone());
            }
            Some(Err(error)) => return Err(stream_failure(error)),
            None => {
                return Err(AttemptFailure::retryable(
                    RuntimeError::CompactionModelStream {
                        message: "compaction model stream ended before completion".to_owned(),
                    },
                ));
            }
        }
    }
}

fn setup_failure(error: ModelError) -> AttemptFailure {
    let kind = error.kind();
    AttemptFailure {
        error: RuntimeError::CompactionModelSetup {
            message: error.message().to_owned(),
        },
        cancelled: kind == ProviderErrorKind::Cancelled,
        retryable: is_retryable_provider_error(kind),
    }
}

fn stream_failure(error: ModelError) -> AttemptFailure {
    let kind = error.kind();
    AttemptFailure {
        error: RuntimeError::CompactionModelStream {
            message: error.message().to_owned(),
        },
        cancelled: kind == ProviderErrorKind::Cancelled,
        retryable: is_retryable_provider_error(kind),
    }
}

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
