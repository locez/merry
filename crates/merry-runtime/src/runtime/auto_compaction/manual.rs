//! One caller-requested reduction, rolling as needed to reach the destination budget.

use super::{
    CompactionAttempt, CompactionProgress, RuntimeInner, compaction_cancelled_before_request,
    compaction_preparation_for_budget, generate_and_install_compaction, manual_compaction_budget,
    plan_compaction_attempt,
};
use crate::{CitationCompactionPolicy, CompactionOutcome, RuntimeError, events::ActiveStepPermit};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
pub(in crate::runtime) async fn compact_context_once_inner(
    inner: &Arc<RuntimeInner>,
    policy: CitationCompactionPolicy,
    token: CancellationToken,
    active_permit: ActiveStepPermit,
) -> Result<Option<CompactionOutcome>, RuntimeError> {
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_request());
    }

    // Manual compaction uses the runtime's compaction reasoning level, not the
    // caller's primary-model generation config, and resolves it once per invocation.
    let reasoning_effort = inner
        .automatic_compaction
        .read()
        .await
        .reasoning_effort()
        .cloned();
    let mut budget = manual_compaction_budget(inner, policy).await?;
    let mut progress = CompactionProgress::new(budget.dynamic_body_estimated_tokens);
    let mut outcome: Option<CompactionOutcome> = None;
    loop {
        if token.is_cancelled() {
            return Err(compaction_cancelled_before_request());
        }
        let Some(preparation) = compaction_preparation_for_budget(inner, &budget).await? else {
            if outcome.is_some() {
                progress.finish(
                    budget.dynamic_body_estimated_tokens,
                    budget.window_budget.max_dynamic_body_tokens(),
                )?;
            }
            return Ok(outcome);
        };
        let plan = match plan_compaction_attempt(
            inner,
            preparation,
            &budget,
            reasoning_effort.as_ref(),
            &token,
        )
        .await?
        {
            CompactionAttempt::ArchiveOnly { reason, .. } => {
                if let Some(error) = reason.budget_failure() {
                    return Err(error);
                }
                if outcome.is_some() {
                    progress.finish(
                        budget.dynamic_body_estimated_tokens,
                        budget.window_budget.max_dynamic_body_tokens(),
                    )?;
                }
                return Ok(outcome);
            }
            CompactionAttempt::Generate(plan) => plan,
        };
        let next = generate_and_install_compaction(
            inner,
            plan,
            &budget,
            reasoning_effort.as_ref(),
            token.clone(),
            &active_permit,
        )
        .await?;
        outcome = Some(match outcome {
            Some(previous) => previous.followed_by(next)?,
            None => next,
        });
        budget = manual_compaction_budget(inner, policy).await?;
        if progress.observe(
            budget.dynamic_body_estimated_tokens,
            budget.target_dynamic_body_tokens(),
            budget.window_budget.max_dynamic_body_tokens(),
            true,
        )? {
            return Ok(outcome);
        }
    }
}
