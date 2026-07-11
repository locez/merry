use super::{
    RuntimeInner, provider_request::resolve_request_context_window, stream_model_with_retry_policy,
};
use crate::{
    CitationCompactionInput, CitationCompactionPolicy, CompactionError, CompactionOutcome,
    ResolvedContextWindow, RuntimeError, RuntimeModelRole,
    compaction::compile_citation_compaction_model_request,
};
use futures_util::StreamExt;
use merry_llm::{FinishReason, ModelEvent, ModelOutput, ModelStreamContext};
use tokio_util::sync::CancellationToken;

pub(super) fn default_automatic_compaction_policy() -> CitationCompactionPolicy {
    CitationCompactionPolicy::default()
}

pub(super) async fn compaction_input_for_hard_watermark(
    inner: &RuntimeInner,
    primary_window: ResolvedContextWindow,
) -> Result<Option<CitationCompactionInput>, RuntimeError> {
    let config = *inner.automatic_compaction.read().await;
    if !config.is_enabled() {
        return Ok(None);
    }

    build_compaction_input(inner, config.policy(), primary_window).await
}

pub(super) async fn compaction_input_for_policy(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
) -> Result<Option<CitationCompactionInput>, RuntimeError> {
    let primary_window = resolved_primary_context_window(inner).await?;
    build_compaction_input(inner, policy, primary_window).await
}

async fn build_compaction_input(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
    primary_window: ResolvedContextWindow,
) -> Result<Option<CitationCompactionInput>, RuntimeError> {
    let resolved_budget = policy.resolve(primary_window.tokens())?;
    let session = inner.session.lock().await;
    session.build_citation_compaction_input(policy, resolved_budget)
}

async fn resolved_primary_context_window(
    inner: &RuntimeInner,
) -> Result<ResolvedContextWindow, RuntimeError> {
    let provider_config = inner.model_config(RuntimeModelRole::Primary).await.ok_or(
        RuntimeError::MissingModelProvider {
            role: RuntimeModelRole::Primary.as_str(),
        },
    )?;
    let context_window_override = inner
        .context_window_tokens
        .read()
        .await
        .map(std::num::NonZeroU64::get);
    resolve_request_context_window(
        provider_config.provider().capabilities(),
        context_window_override,
    )
    .map_err(RuntimeError::from)
}

pub(super) async fn compact_prepared_context(
    inner: &RuntimeInner,
    input: CitationCompactionInput,
    token: CancellationToken,
) -> Result<CompactionOutcome, RuntimeError> {
    if token.is_cancelled() {
        return Err(RuntimeError::Compaction {
            source: CompactionError::InvalidModelResponseShape {
                reason: "compaction cancelled before input build",
            },
        });
    }

    compact_prepared_context_inner(inner, input, token).await
}

pub(super) async fn compact_context_once_inner(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
    token: CancellationToken,
) -> Result<Option<CompactionOutcome>, RuntimeError> {
    if token.is_cancelled() {
        return Err(RuntimeError::Compaction {
            source: CompactionError::InvalidModelResponseShape {
                reason: "compaction cancelled before input build",
            },
        });
    }

    let input = compaction_input_for_policy(inner, policy).await?;
    let Some(input) = input else {
        return Ok(None);
    };

    compact_prepared_context_inner(inner, input, token)
        .await
        .map(Some)
}

async fn compact_prepared_context_inner(
    inner: &RuntimeInner,
    input: CitationCompactionInput,
    token: CancellationToken,
) -> Result<CompactionOutcome, RuntimeError> {
    let provider_config = inner
        .model_config_with_primary_fallback(RuntimeModelRole::ContextCompaction)
        .await
        .ok_or(RuntimeError::MissingModelProvider {
            role: RuntimeModelRole::ContextCompaction.as_str(),
        })?;

    let request = compile_citation_compaction_model_request(&input, provider_config.model())
        .map_err(|error| RuntimeError::CompactionModelRequest {
            message: error.to_string(),
        })?;
    let stream_context =
        ModelStreamContext::new(token.clone()).with_prompt_cache_key(inner.session_id.clone());
    let provider = provider_config.provider();
    let stream = stream_model_with_retry_policy(
        provider,
        provider_config.retry_policy(),
        request,
        stream_context,
        None,
    )
    .await
    .map_err(|error| RuntimeError::CompactionModelSetup {
        message: error.to_string(),
    })?;
    let candidate_json = collect_compaction_candidate_json(stream, token).await?;

    let mut session = inner.session.lock().await;
    session.install_citation_compaction_candidate(input, &candidate_json)
}

async fn collect_compaction_candidate_json(
    mut stream: merry_llm::ModelEventStream,
    token: CancellationToken,
) -> Result<String, RuntimeError> {
    loop {
        let item = tokio::select! {
            biased;
            () = token.cancelled() => {
                return Err(RuntimeError::CompactionModelStream {
                    message: "compaction cancelled while reading model stream".to_owned(),
                });
            }
            item = stream.next() => item,
        };

        match item {
            Some(Ok(ModelEvent::Started)) => {}
            Some(Ok(ModelEvent::OutputTextDelta { .. })) => {}
            Some(Ok(ModelEvent::ToolCallRequested { .. })) => {
                return Err(RuntimeError::CompactionModelStream {
                    message: "compaction model requested a tool call".to_owned(),
                });
            }
            Some(Ok(ModelEvent::Completed { response })) => {
                if response.finish_reason() != FinishReason::Stop {
                    return Err(RuntimeError::CompactionModelStream {
                        message: format!(
                            "compaction model finished with {:?}",
                            response.finish_reason()
                        ),
                    });
                }
                let [ModelOutput::Text { text }] = response.outputs() else {
                    return Err(RuntimeError::CompactionModelStream {
                        message: "compaction model must return exactly one text output".to_owned(),
                    });
                };
                return Ok(text.clone());
            }
            Some(Err(error)) => {
                return Err(RuntimeError::CompactionModelStream {
                    message: error.to_string(),
                });
            }
            None => {
                return Err(RuntimeError::CompactionModelStream {
                    message: "compaction model stream ended before completion".to_owned(),
                });
            }
        }
    }
}
