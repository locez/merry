//! Deterministic compaction window selection and archive-reference planning.

use crate::{
    RuntimeError,
    checkpoint::CheckpointRef,
    compaction::{
        CitationCompactionPolicy, CompactionError, CompactionWindowBudget,
        CompactionWindowFingerprint, CompactionWindowPlan, retained_turn_fallbacks,
    },
    session::{ModelTurnStatus, SessionState, checkpoint_window::history::ModelTurnHistory},
};
use merry_core::ToolCallId;
use std::collections::BTreeSet;

impl SessionState {
    pub(super) fn plan_compaction_window_from_turns(
        &self,
        policy: CitationCompactionPolicy,
        window_budget: CompactionWindowBudget,
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
        let mut saw_completed_turn = false;
        let available_completed = closed_turns
            .iter()
            .filter(|turn| turn.status == ModelTurnStatus::Completed)
            .count();
        for retained_completed_count in
            retained_turn_fallbacks(policy.retained_model_turns(), available_completed)
        {
            let Some(retained_start) =
                retained_start_for_completed_count(closed_turns, retained_completed_count)
            else {
                continue;
            };
            saw_completed_turn = true;
            let candidate_covered = &closed_turns[..retained_start];
            let covered_has_evidence = candidate_covered.iter().any(|turn| !turn.items.is_empty());
            let (covered, raw_turns, base_tokens) = if covered_has_evidence {
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
            };
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
                if covered.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(compaction_window_plan(
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
                    return Ok(Some(compaction_window_plan(
                        covered,
                        raw_turns,
                        archived_tool_call_ids,
                        fingerprint,
                    )?));
                }
            }

            if retained_completed_count == 1 {
                let existing_open_archives = existing_archived_tool_call_ids(open_turns);
                let current_only_tokens = base_tokens
                    .checked_add(projected_turn_tokens(open_turns, &existing_open_archives)?)
                    .ok_or(CompactionError::BudgetOverflow)?;
                return if current_only_tokens >= window_budget.max_dynamic_body_tokens() {
                    Err(CompactionError::UncompressibleCurrentInput.into())
                } else {
                    Err(CompactionError::MinimumRawTurnCannotFit.into())
                };
            }
        }

        if !saw_completed_turn {
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
