use super::{RuntimeInner, provider_request::resolve_request_context_window};
use crate::{
    CitationCompactionInput, CitationCompactionPolicy, CompactionError, CompactionOutcome,
    ResolvedCitationCompactionBudget, ResolvedContextWindow, RuntimeError, RuntimeModelRole,
    compaction::{
        ArchiveOnlyCompactionInput, CompactionPreparation, CompactionWindowBudget,
        compile_citation_compaction_model_request, generate_validated_compaction_candidate,
        validate_compaction_model_window,
    },
    events::ActiveStepPermit,
    session::{PreparedCompactionInstall, SessionState},
    session_store::StagedSessionBundle,
};
use merry_llm::ModelStreamContext;
use std::sync::Arc;
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
    inner: &Arc<RuntimeInner>,
    input: CitationCompactionInput,
    primary_window_tokens: u64,
    token: CancellationToken,
    active_permit: &ActiveStepPermit,
) -> Result<CompactionOutcome, RuntimeError> {
    if token.is_cancelled() {
        return Err(RuntimeError::Compaction {
            source: CompactionError::InvalidModelResponseShape {
                reason: "compaction cancelled before input build",
            },
        });
    }

    compact_prepared_context_inner(inner, input, primary_window_tokens, token, active_permit).await
}

pub(super) async fn compact_context_once_inner(
    inner: &Arc<RuntimeInner>,
    policy: CitationCompactionPolicy,
    token: CancellationToken,
    active_permit: ActiveStepPermit,
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

    compact_prepared_context_inner(inner, input, primary_window.tokens(), token, &active_permit)
        .await
        .map(Some)
}

async fn compact_prepared_context_inner(
    inner: &Arc<RuntimeInner>,
    input: CitationCompactionInput,
    primary_window_tokens: u64,
    token: CancellationToken,
    active_permit: &ActiveStepPermit,
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
    let response_format_name = match request.response_format() {
        Some(merry_llm::ModelResponseFormat::StructuredOutput(format)) => format.name(),
        None => "none",
    };
    tracing::debug!(
        event = "runtime.compaction.request",
        session_id = inner.session_id.as_str(),
        provider_name = provider.name().as_str(),
        model = request.model().as_str(),
        message_count = request.messages().len(),
        estimated_input_tokens = crate::token_estimate::estimate_model_input_tokens(request.input()),
        max_output_tokens = request.generation().max_output_tokens(),
        response_format = response_format_name,
        primary_window_tokens,
        compactor_window_tokens = ?provider.capabilities().max_input_tokens(),
        "compaction model request prepared"
    );
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

    install_citation_compaction_candidate_transactionally(
        Arc::clone(inner),
        input,
        &candidate_json,
        token,
        active_permit.clone(),
    )
    .await
}

pub(super) async fn install_citation_compaction_candidate_transactionally(
    inner: Arc<RuntimeInner>,
    input: CitationCompactionInput,
    candidate_json: &str,
    token: CancellationToken,
    active_permit: ActiveStepPermit,
) -> Result<CompactionOutcome, RuntimeError> {
    let outcome = install_compaction_transaction(inner, &token, active_permit, move |session| {
        session.prepare_citation_compaction_install(input, candidate_json)
    })
    .await?;
    Ok(outcome.expect("prepared checkpoint replacement must carry an outcome"))
}

pub(super) async fn install_archive_only_compaction_transactionally(
    inner: Arc<RuntimeInner>,
    input: ArchiveOnlyCompactionInput,
    token: CancellationToken,
    active_permit: ActiveStepPermit,
) -> Result<(), RuntimeError> {
    let outcome = install_compaction_transaction(inner, &token, active_permit, move |session| {
        session.prepare_archive_only_compaction_install(input)
    })
    .await?;
    debug_assert!(
        outcome.is_none(),
        "prepared archive-only install must not carry an outcome"
    );
    Ok(())
}

async fn install_compaction_transaction(
    inner: Arc<RuntimeInner>,
    token: &CancellationToken,
    active_permit: ActiveStepPermit,
    prepare: impl FnOnce(&SessionState) -> Result<PreparedCompactionInstall, RuntimeError>,
) -> Result<Option<CompactionOutcome>, RuntimeError> {
    let store = inner.session_store.clone();
    let mut session = tokio::select! {
        biased;
        () = token.cancelled() => return Err(compaction_cancelled_before_install()),
        session = inner.session.lock() => session,
    };
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_install());
    }

    let prepared = prepare(&session)?;
    let trajectory_snapshot = inner.trajectory.snapshot();
    let bundle = session.persistable_bundle_with_compaction(&prepared, &trajectory_snapshot)?;
    let Some(store) = store else {
        if token.is_cancelled() {
            return Err(compaction_cancelled_before_install());
        }
        session.revalidate_prepared_compaction_install(&prepared)?;
        if token.is_cancelled() {
            return Err(compaction_cancelled_before_install());
        }
        session.set_trajectory_snapshot(trajectory_snapshot);
        return Ok(session.commit_prepared_compaction_install(prepared));
    };
    drop(session);

    let token = token.clone();
    let trace_token = token.clone();
    let session_id = inner.session_id.clone();
    let commit_task = tokio::spawn(async move {
        let result = async {
            if token.is_cancelled() {
                return Err(compaction_cancelled_before_install());
            }
            let staged = store.stage_bundle(bundle).await?;
            complete_staged_compaction(
                inner,
                staged,
                prepared,
                trajectory_snapshot,
                token,
                active_permit,
            )
            .await
        }
        .await;
        if let Err(error) = &result {
            if matches!(error, RuntimeError::SessionStore { .. }) || !trace_token.is_cancelled() {
                tracing::warn!(
                    session_id = %session_id,
                    error = %error,
                    "compaction transaction task failed"
                );
            } else {
                tracing::debug!(
                    session_id = %session_id,
                    error = %error,
                    "compaction transaction task cancelled"
                );
            }
        }
        result
    });
    commit_task
        .await
        .map_err(|error| RuntimeError::CompactionModelStream {
            message: format!("compaction commit task failed: {error}"),
        })?
}

async fn complete_staged_compaction(
    inner: Arc<RuntimeInner>,
    staged: StagedSessionBundle,
    prepared: PreparedCompactionInstall,
    trajectory_snapshot: merry_core::TrajectorySnapshot,
    token: CancellationToken,
    _active_permit: ActiveStepPermit,
) -> Result<Option<CompactionOutcome>, RuntimeError> {
    if token.is_cancelled() {
        return Err(discard_staged_with_error(staged, compaction_cancelled_before_install()).await);
    }

    if let Err(error) = revalidate_staged_compaction(&inner, &token, &prepared).await {
        return Err(discard_staged_with_error(staged, error).await);
    }

    if token.is_cancelled() {
        return Err(discard_staged_with_error(staged, compaction_cancelled_before_install()).await);
    }
    let commit = staged.commit().await?;
    let mut session = inner.session.lock().await;
    session.set_trajectory_snapshot(trajectory_snapshot);
    let outcome = session.commit_prepared_compaction_install(prepared);
    drop(session);
    commit.require_durable()?;
    Ok(outcome)
}

async fn revalidate_staged_compaction(
    inner: &RuntimeInner,
    token: &CancellationToken,
    prepared: &PreparedCompactionInstall,
) -> Result<(), RuntimeError> {
    let session = tokio::select! {
        biased;
        () = token.cancelled() => return Err(compaction_cancelled_before_install()),
        session = inner.session.lock() => session,
    };
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_install());
    }
    session.revalidate_prepared_compaction_install(prepared)
}

async fn discard_staged_with_error(
    staged: StagedSessionBundle,
    error: RuntimeError,
) -> RuntimeError {
    match staged.discard().await {
        Ok(()) => error,
        Err(discard_error) => discard_error.into(),
    }
}

fn compaction_cancelled_before_install() -> RuntimeError {
    RuntimeError::CompactionModelStream {
        message: "compaction cancelled before checkpoint install".to_owned(),
    }
}
