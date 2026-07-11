use super::{RuntimeInner, provider_request::resolve_request_context_window};
use crate::{
    CitationCompactionInput, CitationCompactionPolicy, CompactionError, CompactionOutcome,
    ResolvedCitationCompactionBudget, ResolvedContextWindow, RuntimeError, RuntimeModelRole,
    compaction::{
        CompactionPreparation, CompactionWindowBudget, compile_citation_compaction_model_request,
        generate_validated_compaction_candidate, validate_compaction_model_window,
    },
};
use merry_llm::ModelStreamContext;
use tokio_util::sync::CancellationToken;

pub(super) fn default_automatic_compaction_policy() -> CitationCompactionPolicy {
    CitationCompactionPolicy::default()
}

pub(super) async fn compaction_preparation_for_hard_watermark(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
    resolved_budget: ResolvedCitationCompactionBudget,
    window_budget: CompactionWindowBudget,
) -> Result<Option<CompactionPreparation>, RuntimeError> {
    let session = inner.session.lock().await;
    session.build_compaction_preparation_with_window_budget(policy, resolved_budget, window_budget)
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
    primary_window_tokens: u64,
    token: CancellationToken,
) -> Result<CompactionOutcome, RuntimeError> {
    if token.is_cancelled() {
        return Err(RuntimeError::Compaction {
            source: CompactionError::InvalidModelResponseShape {
                reason: "compaction cancelled before input build",
            },
        });
    }

    compact_prepared_context_inner(inner, input, primary_window_tokens, token).await
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

    let primary_window = resolved_primary_context_window(inner).await?;
    let input = build_compaction_input(inner, policy, primary_window).await?;
    let Some(input) = input else {
        return Ok(None);
    };

    compact_prepared_context_inner(inner, input, primary_window.tokens(), token)
        .await
        .map(Some)
}

async fn compact_prepared_context_inner(
    inner: &RuntimeInner,
    input: CitationCompactionInput,
    primary_window_tokens: u64,
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
    let provider = provider_config.provider();
    validate_compaction_model_window(
        provider.capabilities(),
        &request,
        primary_window_tokens,
        &inner.session_id,
        provider.name(),
    )?;
    let stream_context =
        ModelStreamContext::new(token.clone()).with_prompt_cache_key(inner.session_id.clone());
    let candidate_json =
        generate_validated_compaction_candidate(provider, request, stream_context, &input, &token)
            .await?;

    let mut session = tokio::select! {
        biased;
        () = token.cancelled() => return Err(compaction_cancelled_before_install()),
        session = inner.session.lock() => session,
    };
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_install());
    }
    session.install_citation_compaction_candidate(input, &candidate_json)
}

fn compaction_cancelled_before_install() -> RuntimeError {
    RuntimeError::CompactionModelStream {
        message: "compaction cancelled before checkpoint install".to_owned(),
    }
}
