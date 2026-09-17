//! Manual, single-pass compaction requested by a caller.

use super::{
    CompactionAttempt, RuntimeInner, compaction_cancelled_before_request,
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
    // caller's primary-model generation config, and resolves it once per pass.
    let reasoning_effort = inner
        .automatic_compaction
        .read()
        .await
        .reasoning_effort()
        .cloned();
    let budget = manual_compaction_budget(inner, policy).await?;
    let Some((preparation, budget)) = compaction_preparation_for_budget(inner, budget).await?
    else {
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
