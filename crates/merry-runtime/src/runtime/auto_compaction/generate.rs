//! Generating and installing one compaction candidate.

use super::fit::ReservePolicy;
use super::install::install_citation_compaction_candidate_transactionally;
use super::plan::{MAX_COMPACTION_TRUNCATION_REFITS, fit_compaction_plan};
use super::{
    CompactionAttempt, CompactionPlan, CompactionRequestBudget, RuntimeInner,
    compaction_cancelled_before_request,
};
use crate::{
    CompactionOutcome, RuntimeError, RuntimeModelRole,
    compaction::{CompactionCoverageBudget, generate_validated_compaction_candidate},
    events::ActiveStepPermit,
};
use merry_llm::{ModelStreamContext, ReasoningEffort};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
pub(crate) async fn generate_and_install_compaction(
    inner: &Arc<RuntimeInner>,
    plan: CompactionPlan,
    budget: &CompactionRequestBudget,
    reasoning_effort: Option<&ReasoningEffort>,
    token: CancellationToken,
    active_permit: &ActiveStepPermit,
) -> Result<CompactionOutcome, RuntimeError> {
    let provider_config = inner
        .model_config_with_primary_fallback(RuntimeModelRole::ContextCompaction)
        .await
        .ok_or(RuntimeError::MissingModelProvider {
            role: RuntimeModelRole::ContextCompaction.as_str(),
        })?;
    let provider = provider_config.provider();
    let mut plan = plan;
    let mut attempt = 0;
    loop {
        attempt += 1;
        if token.is_cancelled() {
            return Err(compaction_cancelled_before_request());
        }
        let stream_context =
            ModelStreamContext::new(token.clone()).with_prompt_cache_key(inner.session_id.clone());
        match generate_validated_compaction_candidate(
            provider.clone(),
            plan.request.as_ref().clone(),
            stream_context,
            &plan.input,
            &token,
        )
        .await
        {
            Ok(candidate_json) => {
                return install_citation_compaction_candidate_transactionally(
                    Arc::clone(inner),
                    *plan.input,
                    &candidate_json,
                    token,
                    active_permit.clone(),
                )
                .await;
            }
            Err(RuntimeError::CompactionModelTruncated { message }) => {
                if attempt > MAX_COMPACTION_TRUNCATION_REFITS {
                    return Err(RuntimeError::CompactionModelTruncated { message });
                }
                let next_reserve = plan.reserve.degraded();
                if next_reserve == plan.reserve {
                    return Err(RuntimeError::CompactionModelTruncated { message });
                }
                // The reserve grew, so the covered window has to shrink for the
                // window to host it. Re-planning from the untightened budget lets
                // the fit loop find that covered window.
                let rebuilt = {
                    let session = inner.session.lock().await;
                    session.build_rolling_compaction_preparation(
                        budget.policy,
                        budget.resolved_budget,
                        budget.window_budget,
                        CompactionCoverageBudget::unbounded(),
                    )?
                };
                let Some(rebuilt) = rebuilt else {
                    return Err(RuntimeError::CompactionModelTruncated { message });
                };
                let CompactionAttempt::Generate(next_plan) = fit_compaction_plan(
                    inner,
                    rebuilt,
                    budget,
                    reasoning_effort,
                    next_reserve,
                    ReservePolicy::Required,
                    &token,
                )
                .await?
                else {
                    // Archiving tool results cannot fix a truncated checkpoint, and
                    // installing it here would silently change the reduction the
                    // caller announced.
                    return Err(RuntimeError::CompactionModelTruncated { message });
                };
                tracing::debug!(
                    event = "runtime.compaction.truncation_refit",
                    session_id = inner.session_id.as_str(),
                    attempt,
                    reserve_percent = next_reserve.percent(),
                    message,
                    "compaction output was truncated; retrying with a larger reasoning reserve and a smaller covered window"
                );
                plan = next_plan;
            }
            Err(error) => return Err(error),
        }
    }
}
