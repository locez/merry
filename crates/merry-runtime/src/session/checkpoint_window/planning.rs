//! Deterministic compaction window selection and archive-reference planning.

use crate::{
    RuntimeError,
    checkpoint::CheckpointRef,
    compaction::{
        CitationCompactionPolicy, CompactionCoverageBudget, CompactionError, CompactionShape,
        CompactionWindowBudget, CompactionWindowFingerprint, CompactionWindowPlan, RetainedFit,
        retained_turn_fallbacks,
    },
    session::{
        ModelTurnStatus, SessionState,
        checkpoint_window::history::{ModelTurnHistory, covered_payload_tokens},
    },
};
use merry_core::ToolCallId;
use std::collections::BTreeSet;

/// One retention option the planner evaluates.
///
/// `CompletedTurns` keeps that many completed turns raw and covers everything
/// older. `ArchiveOnly` keeps every turn raw and only archives tool results; the
/// runtime installs it without another model call, which is the degradation path
/// when no checkpoint replacement fits the compaction request budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetentionCandidate {
    CompletedTurns(usize),
    ArchiveOnly,
}

impl RetentionCandidate {
    /// Returns what an empty covered window means for this candidate.
    fn empty_coverage_meaning(self) -> EmptyCoverage {
        match self {
            Self::CompletedTurns(_) => EmptyCoverage::NothingToDo,
            // Archiving tool results is a real reduction, so the empty covered set
            // is the requested plan rather than a no-op.
            Self::ArchiveOnly => EmptyCoverage::Reduction,
        }
    }
}

/// What an empty covered window means for one retention candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmptyCoverage {
    /// Nothing to summarize: the caller reports that no compression applies.
    NothingToDo,
    /// Archive-only reduction: the empty covered set is the plan.
    Reduction,
}

/// Result of evaluating one retention candidate.
enum CandidateOutcome {
    Plan(CompactionWindowPlan),
    NothingToDo,
    DoesNotFit,
}

impl SessionState {
    pub(super) fn plan_compaction_window_from_turns(
        &self,
        policy: CitationCompactionPolicy,
        window_budget: CompactionWindowBudget,
        coverage: CompactionCoverageBudget,
        shape: CompactionShape,
        turns: &[ModelTurnHistory],
    ) -> Result<Option<CompactionWindowPlan>, RuntimeError> {
        debug_assert!(
            window_budget.max_dynamic_body_tokens() <= window_budget.primary_window_tokens()
        );
        let first_open = turns
            .iter()
            .position(|turn| turn.status.is_open())
            .unwrap_or(turns.len());
        if turns[first_open..]
            .iter()
            .any(|turn| !turn.status.is_open())
        {
            return Err(CompactionError::StaleWindow.into());
        }
        let closed_turns = &turns[..first_open];
        let open_turns = &turns[first_open..];

        let fingerprint = self.compaction_window_fingerprint()?;
        let available_completed = closed_turns
            .iter()
            .filter(|turn| turn.status == ModelTurnStatus::Completed)
            .count();
        let mut candidates =
            self.retention_candidates(policy, coverage, shape, closed_turns, available_completed)?;
        // A coverage budget only exists when the runtime already knows a
        // checkpoint replacement does not fit its request. Archiving tool results
        // is then the remaining degradation, because it reduces the request body
        // without spending another model call.
        let bounded_coverage = coverage.max_tokens().is_some();
        if bounded_coverage {
            candidates.push(RetentionCandidate::ArchiveOnly);
        }

        let mut last_failure: Option<CompactionError> = None;
        for candidate in candidates {
            let (covered, raw_turns, base_tokens) = match candidate {
                RetentionCandidate::CompletedTurns(retained_completed_count) => {
                    let Some(retained_start) =
                        retained_start_for_completed_count(closed_turns, retained_completed_count)
                    else {
                        continue;
                    };
                    let candidate_covered = &closed_turns[..retained_start];
                    if candidate_covered.iter().any(|turn| !turn.items.is_empty()) {
                        (
                            candidate_covered,
                            &turns[retained_start..],
                            window_budget
                                .replacement_fixed_dynamic_body_tokens()
                                .checked_add(window_budget.checkpoint_output_ceiling_tokens())
                                .ok_or(CompactionError::BudgetOverflow)?,
                        )
                    } else {
                        (
                            &closed_turns[..0],
                            turns,
                            window_budget.archive_only_fixed_dynamic_body_tokens(),
                        )
                    }
                }
                RetentionCandidate::ArchiveOnly => (
                    &closed_turns[..0],
                    turns,
                    window_budget.archive_only_fixed_dynamic_body_tokens(),
                ),
            };

            match plan_retained_window(
                window_budget,
                covered,
                raw_turns,
                base_tokens,
                fingerprint,
                candidate.empty_coverage_meaning(),
                shape.retained_fit(),
            )? {
                CandidateOutcome::Plan(plan) => return Ok(Some(plan)),
                CandidateOutcome::NothingToDo => return Ok(None),
                CandidateOutcome::DoesNotFit => {
                    if matches!(candidate, RetentionCandidate::CompletedTurns(1)) {
                        let existing_open_archives = existing_archived_tool_call_ids(open_turns);
                        let current_only_tokens = base_tokens
                            .checked_add(projected_turn_tokens(
                                open_turns,
                                &existing_open_archives,
                            )?)
                            .ok_or(CompactionError::BudgetOverflow)?;
                        let error =
                            if current_only_tokens >= window_budget.max_dynamic_body_tokens() {
                                CompactionError::UncompressibleCurrentInput
                            } else {
                                CompactionError::MinimumRawTurnCannotFit
                            };
                        if !bounded_coverage {
                            return Err(error.into());
                        }
                        last_failure = Some(error);
                    } else if last_failure.is_none() {
                        last_failure = Some(CompactionError::NoWindowFitsCompactionRequest);
                    }
                }
            }
        }

        match last_failure {
            Some(error) => Err(error.into()),
            None => {
                // Every retention candidate needs one completed turn to retain,
                // so an empty candidate list means the history holds no completed
                // turn at all. That case can still be uncompressible when the open
                // turns alone exceed the hard watermark.
                if available_completed == 0 {
                    let existing_open_archives = existing_archived_tool_call_ids(open_turns);
                    let current_only_tokens = window_budget
                        .archive_only_fixed_dynamic_body_tokens()
                        .checked_add(projected_turn_tokens(open_turns, &existing_open_archives)?)
                        .ok_or(CompactionError::BudgetOverflow)?;
                    if current_only_tokens >= window_budget.max_dynamic_body_tokens() {
                        return Err(CompactionError::UncompressibleCurrentInput.into());
                    }
                }
                Ok(None)
            }
        }
    }

    /// Returns the retention options to evaluate, in preference order.
    ///
    /// Without a coverage budget the planner keeps the configured retention and
    /// falls back to smaller raw tails when the request body does not fit. With a
    /// budget, the covered window itself must fit one compaction request, so the
    /// planner retains more completed turns until the covered payload fits;
    /// covering less than the configured retention would only grow the payload the
    /// budget just rejected.
    fn retention_candidates(
        &self,
        policy: CitationCompactionPolicy,
        coverage: CompactionCoverageBudget,
        shape: CompactionShape,
        closed_turns: &[ModelTurnHistory],
        available_completed: usize,
    ) -> Result<Vec<RetentionCandidate>, RuntimeError> {
        if shape.is_one_shot() {
            // One pass covers everything before the retained tail, so the only
            // candidate is the configured retention. The fallbacks below retain
            // fewer turns, which would cover more history and grow the payload the
            // one-shot pass is trying to fit.
            return Ok(vec![RetentionCandidate::CompletedTurns(
                policy
                    .retained_model_turns()
                    .min(available_completed)
                    .max(1),
            )]);
        }
        let Some(coverage_budget) = coverage.max_tokens() else {
            return Ok(
                retained_turn_fallbacks(policy.retained_model_turns(), available_completed)
                    .into_iter()
                    .map(RetentionCandidate::CompletedTurns)
                    .collect(),
            );
        };
        let configured = policy
            .retained_model_turns()
            .min(available_completed)
            .max(1);
        for retained_completed_count in configured..=available_completed {
            let Some(retained_start) =
                retained_start_for_completed_count(closed_turns, retained_completed_count)
            else {
                continue;
            };
            if covered_payload_tokens(&closed_turns[..retained_start])? <= coverage_budget {
                return Ok(vec![RetentionCandidate::CompletedTurns(
                    retained_completed_count,
                )]);
            }
        }
        Ok(Vec::new())
    }
}

/// Evaluates one covered/retained split against the request body budget.
///
/// Returns the plan when the split fits, `NothingToDo` when an empty covered set
/// means there is nothing to summarize, and `DoesNotFit` when neither the split
/// nor additional tool-result archiving brings the projection below the hard
/// watermark. `empty_coverage` carries what an empty covered set means for the
/// candidate being evaluated.
fn plan_retained_window(
    window_budget: CompactionWindowBudget,
    covered: &[ModelTurnHistory],
    raw_turns: &[ModelTurnHistory],
    base_tokens: u64,
    fingerprint: CompactionWindowFingerprint,
    empty_coverage: EmptyCoverage,
    retained_fit: RetainedFit,
) -> Result<CandidateOutcome, RuntimeError> {
    let mut archived_tool_call_ids = existing_archived_tool_call_ids(raw_turns);
    let fits = |archived_tool_call_ids: &BTreeSet<ToolCallId>| {
        retained_projection_fits(
            base_tokens,
            raw_turns,
            archived_tool_call_ids,
            window_budget.max_dynamic_body_tokens(),
        )
    };

    if fits(&archived_tool_call_ids)? {
        if covered.is_empty() && empty_coverage == EmptyCoverage::NothingToDo {
            return Ok(CandidateOutcome::NothingToDo);
        }
        return Ok(CandidateOutcome::Plan(compaction_window_plan(
            covered,
            raw_turns,
            archived_tool_call_ids,
            fingerprint,
        )?));
    }

    let mut archive_candidates = raw_turns
        .iter()
        .flat_map(ModelTurnHistory::archive_candidates_in_result_order)
        .collect::<Vec<_>>();
    archive_candidates.sort_by_key(|(result_item_id, _)| *result_item_id);
    for (_, call_id) in archive_candidates {
        archived_tool_call_ids.insert(call_id);
        if fits(&archived_tool_call_ids)? {
            return Ok(CandidateOutcome::Plan(compaction_window_plan(
                covered,
                raw_turns,
                archived_tool_call_ids,
                fingerprint,
            )?));
        }
    }

    match retained_fit {
        RetainedFit::Required => Ok(CandidateOutcome::DoesNotFit),
        // Another pass follows, so install the largest covered window instead of
        // reporting that nothing fits. The wait for the budget to hold moves to the
        // caller, which recompiles and decides whether to run one more pass.
        RetainedFit::Deferred => Ok(CandidateOutcome::Plan(compaction_window_plan(
            covered,
            raw_turns,
            existing_archived_tool_call_ids(raw_turns),
            fingerprint,
        )?)),
    }
}

pub(super) fn retained_start_for_completed_count(
    closed_turns: &[ModelTurnHistory],
    retained_completed_count: usize,
) -> Option<usize> {
    let mut completed_seen = 0;
    closed_turns
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, turn)| {
            if turn.status == ModelTurnStatus::Completed {
                completed_seen += 1;
                (completed_seen == retained_completed_count).then_some(index)
            } else {
                None
            }
        })
}

pub(super) fn existing_archived_tool_call_ids(turns: &[ModelTurnHistory]) -> BTreeSet<ToolCallId> {
    turns
        .iter()
        .flat_map(ModelTurnHistory::existing_archived_tool_call_ids)
        .collect()
}

pub(super) fn archived_refs_for_plan(
    turns: &[ModelTurnHistory],
    plan: &CompactionWindowPlan,
) -> Result<Vec<CheckpointRef>, RuntimeError> {
    let mut found_call_ids = BTreeSet::new();
    let mut refs = Vec::new();
    for turn in turns {
        if !plan.retained_turn_ids().contains(&turn.id) {
            continue;
        }
        for record in &turn.items {
            let Some((result_item_id, call_id, _)) = record.item.tool_result_archive_candidate()
            else {
                continue;
            };
            if plan.archived_tool_call_ids().contains(&call_id) {
                found_call_ids.insert(call_id);
                refs.push((result_item_id, record.reference.clone()));
            }
        }
    }
    if found_call_ids != *plan.archived_tool_call_ids() {
        return Err(CompactionError::StaleWindow.into());
    }
    refs.sort_by_key(|(result_item_id, _)| *result_item_id);
    Ok(refs.into_iter().map(|(_, reference)| reference).collect())
}

pub(super) fn projected_turn_tokens(
    turns: &[ModelTurnHistory],
    archived_tool_call_ids: &BTreeSet<ToolCallId>,
) -> Result<u64, RuntimeError> {
    turns.iter().try_fold(0_u64, |total, turn| {
        total
            .checked_add(turn.projected_token_estimate(archived_tool_call_ids)?)
            .ok_or_else(|| RuntimeError::from(CompactionError::BudgetOverflow))
    })
}

pub(super) fn retained_projection_fits(
    base_tokens: u64,
    raw_turns: &[ModelTurnHistory],
    archived_tool_call_ids: &BTreeSet<ToolCallId>,
    max_dynamic_body_tokens: u64,
) -> Result<bool, RuntimeError> {
    Ok(base_tokens
        .checked_add(projected_turn_tokens(raw_turns, archived_tool_call_ids)?)
        .ok_or(CompactionError::BudgetOverflow)?
        < max_dynamic_body_tokens)
}

pub(super) fn compaction_window_plan(
    covered: &[ModelTurnHistory],
    raw_turns: &[ModelTurnHistory],
    archived_tool_call_ids: BTreeSet<ToolCallId>,
    fingerprint: CompactionWindowFingerprint,
) -> Result<CompactionWindowPlan, RuntimeError> {
    Ok(CompactionWindowPlan::new(
        covered.iter().map(|turn| turn.id).collect(),
        raw_turns.iter().map(|turn| turn.id).collect(),
        archived_tool_call_ids,
        covered.last().map(|turn| turn.id),
        fingerprint,
    ))
}
