//! Choosing a covered window the compaction model window can host.

use super::super::RuntimeInner;
use super::fit::{
    CompactionModelLimits, CompactionRequestFit, ReservePolicy, compile_fitted_compaction_request,
    trace_compaction_request,
};
use super::{
    ArchiveOnlyReason, CompactionAttempt, CompactionPlan, CompactionRequestBudget,
    build_preparation_for_shape, compaction_cancelled_before_request, compaction_stable_prefix,
};
use crate::{
    CompactionError, RuntimeError, RuntimeModelRole,
    compaction::{
        CompactionCoverageBudget, CompactionPreparation, CompactionReasoningReserve,
        CompactionShape, compaction_model_window, tightened_covered_budget,
        validate_compaction_model_window,
    },
};
use merry_llm::ReasoningEffort;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
pub(in crate::runtime) async fn plan_compaction_attempt(
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
pub(super) const MAX_COMPACTION_TRUNCATION_REFITS: usize = 1;
/// Fits one prepared compaction under a specific reasoning reserve.
///
/// A request is only returned when the compaction model window can host its input
/// and the output budget `policy` requires, so the provider is never asked for
/// output it cannot deliver. Otherwise the covered window shrinks and the planner
/// re-runs; when no covered window fits, the planner degrades to archiving tool
/// results, which the caller installs or reports.
pub(super) async fn fit_compaction_plan(
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
    // The shape the loop is currently building, which starts at the strategy the
    // step chose and can fall back to rolling when one pass cannot fit.
    let mut shape = budget.shape;
    let mut attempt = 0;
    // Coverage tightening is what this bounds. Changing the shape is progress of a
    // different kind, so it does not consume the budget.
    let mut tightening_attempts = 0;
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
                // A one-shot pass shrinks the payload by omitting covered tool
                // exchanges before it considers covering less history: covering less
                // would keep more raw history, which is the state this strategy exists
                // to leave.
                if shape.is_one_shot() {
                    let next_shape = match shape {
                        CompactionShape::OneShot {
                            retained_tool_exchanges,
                        } if retained_tool_exchanges > 0 => shape.with_all_tool_exchanges_dropped(),
                        _ => {
                            // Every covered tool exchange is already omitted, so this
                            // history cannot fit one pass. Fall back to rolling, which
                            // covers less history per pass and repeats.
                            CompactionShape::Rolling
                        }
                    };
                    // Changing the shape is progress even when the estimate does not
                    // move, so the rolling no-progress guard starts over.
                    previous_input_tokens = None;
                    if next_shape.is_one_shot() {
                        tracing::debug!(
                            event = "runtime.compaction.one_shot_refit",
                            session_id = inner.session_id.as_str(),
                            attempt,
                            estimated_input_tokens,
                            max_output_tokens,
                            "one-shot payload does not fit; omitting every covered tool exchange"
                        );
                    }
                    shape = next_shape;
                    let Some(rebuilt) = rebuild_preparation(
                        inner,
                        budget,
                        shape,
                        CompactionCoverageBudget::unbounded(),
                    )
                    .await?
                    else {
                        return Err(too_large);
                    };
                    preparation = rebuilt;
                    continue;
                }
                // Rolling cannot shrink the request any further by rebuilding the
                // same plan, so report the budget failure instead of repeating it.
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
                tightening_attempts += 1;
                if tightening_attempts > MAX_COMPACTION_FIT_ATTEMPTS {
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
                let Some(rebuilt) = rebuild_preparation(inner, budget, shape, coverage).await?
                else {
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

/// Rebuilds the preparation for one shape and coverage.
async fn rebuild_preparation(
    inner: &Arc<RuntimeInner>,
    budget: &CompactionRequestBudget,
    shape: CompactionShape,
    coverage: CompactionCoverageBudget,
) -> Result<Option<CompactionPreparation>, RuntimeError> {
    let session = inner.session.lock().await;
    build_preparation_for_shape(
        &session,
        budget.policy,
        budget.resolved_budget,
        budget.window_budget,
        shape,
        coverage,
    )
}
