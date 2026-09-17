//! Choosing a covered window the compaction model window can host.

use super::super::RuntimeInner;
use super::fit::{
    CompactionModelLimits, CompactionRequestFit, ReservePolicy, compile_fitted_compaction_request,
    trace_compaction_request,
};
use super::{
    ArchiveOnlyReason, CompactionAttempt, CompactionPlan, CompactionRequestBudget,
    compaction_cancelled_before_request, compaction_stable_prefix,
};
use crate::{
    CompactionError, RuntimeError, RuntimeModelRole,
    compaction::{
        CompactionCoverageBudget, CompactionPreparation, CompactionReasoningReserve,
        compaction_model_window, tightened_covered_budget, validate_compaction_model_window,
    },
};
use merry_llm::ReasoningEffort;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
pub(crate) async fn plan_compaction_attempt(
    inner: &Arc<RuntimeInner>,
    preparation: CompactionPreparation,
    budget: &CompactionRequestBudget,
    reasoning_effort: Option<&ReasoningEffort>,
    token: &CancellationToken,
) -> Result<CompactionAttempt, RuntimeError> {
    fit_compaction_plan(
        inner,
        preparation,
        budget,
        reasoning_effort,
        CompactionReasoningReserve::INITIAL,
        ReservePolicy::BestEffort,
        token,
    )
    .await
}

/// Fit attempts one prepared compaction may spend before reporting a budget failure.
const MAX_COMPACTION_FIT_ATTEMPTS: usize = 3;
/// Provider calls one truncated compaction may spend before failing: at most one
/// degraded re-plan on top of the original attempt.
pub(crate) const MAX_COMPACTION_TRUNCATION_REFITS: usize = 1;
/// Fits one prepared compaction under a specific reasoning reserve.
///
/// A request is only returned when the compaction model window can host its input
/// and the output budget `policy` requires, so the provider is never asked for
/// output it cannot deliver. Otherwise the covered window shrinks and the planner
/// re-runs; when no covered window fits, the planner degrades to archiving tool
/// results, which the caller installs or reports.
pub(crate) async fn fit_compaction_plan(
    inner: &Arc<RuntimeInner>,
    preparation: CompactionPreparation,
    budget: &CompactionRequestBudget,
    reasoning_effort: Option<&ReasoningEffort>,
    reserve: CompactionReasoningReserve,
    policy: ReservePolicy,
    token: &CancellationToken,
) -> Result<CompactionAttempt, RuntimeError> {
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_request());
    }
    let provider_config = inner
        .model_config_with_primary_fallback(RuntimeModelRole::ContextCompaction)
        .await
        .ok_or(RuntimeError::MissingModelProvider {
            role: RuntimeModelRole::ContextCompaction.as_str(),
        })?;
    let provider = provider_config.provider();
    let compactor_window_tokens = compaction_model_window(
        provider.capabilities(),
        budget.primary_window_tokens,
        &inner.session_id,
        provider.name(),
    )?;
    let limits = CompactionModelLimits {
        window_tokens: compactor_window_tokens,
        max_output_tokens: provider.capabilities().max_output_tokens(),
    };
    let stable_prefix = compaction_stable_prefix(inner).await?;

    let mut preparation = preparation;
    let mut attempt = 0;
    let mut tightened_coverage = false;
    let mut previous_input_tokens: Option<u64> = None;
    let mut smallest_rejected_request: Option<(u64, u64)> = None;
    loop {
        attempt += 1;
        let input = match preparation {
            CompactionPreparation::ArchiveToolResults(input) => {
                let reason = if tightened_coverage {
                    let Some((estimated_input_tokens, max_output_tokens)) =
                        smallest_rejected_request
                    else {
                        return Err(RuntimeError::Compaction {
                            source: CompactionError::InvalidModelResponseShape {
                                reason: "compaction refit lost its rejection record",
                            },
                        });
                    };
                    ArchiveOnlyReason::BudgetExhausted {
                        estimated_input_tokens,
                        max_output_tokens,
                        compactor_window_tokens,
                    }
                } else {
                    ArchiveOnlyReason::PlanChoseArchiveOnly
                };
                tracing::debug!(
                    event = "runtime.compaction.archive_only_requested",
                    session_id = inner.session_id.as_str(),
                    attempt,
                    ?reason,
                    "compaction keeps every turn raw and archives tool results instead"
                );
                return Ok(CompactionAttempt::ArchiveOnly { input, reason });
            }
            CompactionPreparation::ReplaceCheckpoint(input) => *input,
        };
        let request = match compile_fitted_compaction_request(
            &input,
            provider_config.model(),
            &stable_prefix,
            reasoning_effort,
            limits,
            reserve,
            policy,
        )? {
            CompactionRequestFit::Request { request, .. } => request,
            CompactionRequestFit::WindowTooSmall {
                estimated_input_tokens,
                max_output_tokens,
            } => {
                let too_large = RuntimeError::CompactionModelRequestTooLarge {
                    estimated_input_tokens,
                    max_output_tokens,
                    compactor_window_tokens,
                };
                smallest_rejected_request = Some((estimated_input_tokens, max_output_tokens));
                // Re-planning cannot shrink the request any further, so report the
                // budget failure instead of repeating the same plan.
                if previous_input_tokens == Some(estimated_input_tokens) {
                    return Err(too_large);
                }
                let covered_payload_tokens = input
                    .covered_payload_token_estimate()
                    .map_err(|source| RuntimeError::Compaction { source })?;
                // The reserve is a share of the request input, so giving up one
                // token of covered history frees its own reserve as well. Solve
                // for the input the window can host instead of subtracting the
                // raw overshoot, which would give up far more history than needed.
                let allowed_input_tokens = reserve.allowed_input_tokens(
                    compactor_window_tokens,
                    input.resolved_budget().output_token_limit(),
                );
                let Some(tightened) = tightened_covered_budget(
                    covered_payload_tokens,
                    estimated_input_tokens,
                    allowed_input_tokens,
                ) else {
                    return Err(too_large);
                };
                if attempt >= MAX_COMPACTION_FIT_ATTEMPTS {
                    return Err(too_large);
                }
                tracing::debug!(
                    event = "runtime.compaction.request_refit",
                    session_id = inner.session_id.as_str(),
                    attempt,
                    compactor_window_tokens,
                    estimated_input_tokens,
                    max_output_tokens,
                    covered_payload_tokens,
                    tightened_covered_payload_tokens = tightened,
                    "compaction window cannot host the checkpoint text budget and reasoning reserve; retaining more raw history"
                );
                let coverage = CompactionCoverageBudget::limited(tightened);
                tightened_coverage = true;
                previous_input_tokens = Some(estimated_input_tokens);
                let rebuilt = {
                    let session = inner.session.lock().await;
                    session.build_rolling_compaction_preparation(
                        budget.policy,
                        budget.resolved_budget,
                        budget.window_budget,
                        coverage,
                    )?
                };
                let Some(rebuilt) = rebuilt else {
                    return Err(too_large);
                };
                preparation = rebuilt;
                continue;
            }
        };
        trace_compaction_request(inner, provider.as_ref(), &request, budget, attempt);
        // One gate owns the invariant: the fitter only decides which covered
        // window to try, and this check decides whether the request may be sent.
        validate_compaction_model_window(&request, compactor_window_tokens)?;
        return Ok(CompactionAttempt::Generate(CompactionPlan {
            input: Box::new(input),
            request,
            reserve,
        }));
    }
}
