//! Manual, single-pass compaction requested by a caller.

use super::{
    CompactionAttempt, CompactionRequestBudget, RuntimeInner, compaction_cancelled_before_request,
    generate_and_install_compaction, plan_compaction_attempt, resolved_primary_context_window,
};
use crate::{
    CitationCompactionPolicy, CompactionOutcome, RuntimeError,
    compaction::{CompactionCoverageBudget, CompactionWindowBudget},
    events::ActiveStepPermit,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
pub(crate) async fn compact_context_once_inner(
    inner: &Arc<RuntimeInner>,
    policy: CitationCompactionPolicy,
    token: CancellationToken,
    active_permit: ActiveStepPermit,
) -> Result<Option<CompactionOutcome>, RuntimeError> {
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_request());
    }

    // Manual compaction uses the runtime's compaction reasoning level, not the
    // caller's primary-model generation config, and resolves it once per pass.
    let reasoning_effort = inner
        .automatic_compaction
        .read()
        .await
        .reasoning_effort()
        .cloned();
    let primary_window = resolved_primary_context_window(inner).await?;
    let resolved_budget = policy.resolve(primary_window.tokens())?;
    let window_budget = CompactionWindowBudget::unbounded_for_manual_compaction(
        resolved_budget.output_token_limit(),
    )?;
    let budget = CompactionRequestBudget {
        policy,
        resolved_budget,
        window_budget,
        primary_window_tokens: primary_window.tokens(),
    };
    let preparation = {
        let session = inner.session.lock().await;
        session.build_compaction_preparation_with_window_budget(
            policy,
            resolved_budget,
            window_budget,
            CompactionCoverageBudget::unbounded(),
        )?
    };
    let Some(preparation) = preparation else {
        return Ok(None);
    };

    match plan_compaction_attempt(
        inner,
        preparation,
        &budget,
        reasoning_effort.as_ref(),
        &token,
    )
    .await?
    {
        // Manual compaction keeps its existing contract for the planner's own
        // archive-only choice, but reports an unaffordable request as a failure:
        // the caller asked to compact and the compaction window cannot host any
        // checkpoint replacement.
        CompactionAttempt::ArchiveOnly { reason, .. } => match reason.budget_failure() {
            Some(error) => Err(error),
            None => Ok(None),
        },
        CompactionAttempt::Generate(plan) => generate_and_install_compaction(
            inner,
            plan,
            &budget,
            reasoning_effort.as_ref(),
            token,
            &active_permit,
        )
        .await
        .map(Some),
    }
}
