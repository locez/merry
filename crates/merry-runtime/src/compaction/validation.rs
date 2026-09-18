//! Checkpoint validation and content-free accounting after restoring kept entries.

use super::{CitationCompactionInput, CompactionError};
use crate::{
    RuntimeError,
    checkpoint::{
        CheckpointError, CheckpointHandoff, CheckpointId, CheckpointValidationPolicy,
        CitationBackedCheckpoint, CompactedCheckpointCandidate,
    },
    token_estimate::estimate_text_tokens,
};
use serde::Serialize;

/// Numeric measurements only; safe to log and send as repair feedback.
#[derive(Debug, Clone, Copy, Serialize)]
pub(super) struct CandidateMetrics {
    pub(super) candidate_bytes: usize,
    pub(super) rendered_summary_tokens: Option<u64>,
    pub(super) previous_summary_tokens: u64,
    pub(super) kept_entry_count: usize,
    pub(super) kept_entry_tokens: u64,
    pub(super) soft_target_tokens: u64,
    pub(super) hard_limit_tokens: u64,
    pub(super) max_candidate_bytes: usize,
}

impl CandidateMetrics {
    pub(super) fn trace(self, session_id: &merry_core::SessionId, attempt: usize, accepted: bool) {
        tracing::debug!(
            event = "runtime.compaction.candidate_evaluated",
            session_id = session_id.as_str(),
            attempt,
            accepted,
            candidate_bytes = self.candidate_bytes,
            rendered_summary_tokens = self.rendered_summary_tokens,
            previous_summary_tokens = self.previous_summary_tokens,
            kept_entry_count = self.kept_entry_count,
            kept_entry_tokens = self.kept_entry_tokens,
            soft_target_tokens = self.soft_target_tokens,
            hard_limit_tokens = self.hard_limit_tokens,
            max_candidate_bytes = self.max_candidate_bytes,
            "compaction candidate measured after restoring kept entries"
        );
    }
}

pub(super) struct CandidateEvaluation {
    pub(super) result: Result<CitationBackedCheckpoint, RuntimeError>,
    pub(super) metrics: CandidateMetrics,
}

pub(super) fn evaluate_candidate(
    checkpoint_id: CheckpointId,
    input: &CitationCompactionInput,
    candidate_json: &str,
) -> CandidateEvaluation {
    let budget = input.resolved_budget();
    let mut metrics = CandidateMetrics {
        candidate_bytes: candidate_json.len(),
        rendered_summary_tokens: None,
        previous_summary_tokens: input
            .payload
            .previous_checkpoint
            .as_ref()
            .map_or(0, |previous| previous.estimated_tokens),
        kept_entry_count: 0,
        kept_entry_tokens: 0,
        soft_target_tokens: budget.target_output_tokens(),
        hard_limit_tokens: budget.output_token_limit(),
        max_candidate_bytes: budget.max_accepted_output_bytes(),
    };
    let result = build_checkpoint(checkpoint_id, input, candidate_json, &mut metrics);
    CandidateEvaluation { result, metrics }
}

pub(crate) fn checkpoint_from_candidate_json(
    checkpoint_id: CheckpointId,
    input: &CitationCompactionInput,
    candidate_json: &str,
) -> Result<CitationBackedCheckpoint, RuntimeError> {
    evaluate_candidate(checkpoint_id, input, candidate_json).result
}

fn build_checkpoint(
    checkpoint_id: CheckpointId,
    input: &CitationCompactionInput,
    candidate_json: &str,
    metrics: &mut CandidateMetrics,
) -> Result<CitationBackedCheckpoint, RuntimeError> {
    if candidate_json.len() > input.resolved_budget().max_accepted_output_bytes() {
        return Err(CheckpointError::OutputTooLarge {
            actual_bytes: candidate_json.len(),
            max_bytes: input.resolved_budget().max_accepted_output_bytes(),
        }
        .into());
    }

    let mut candidate = CompactedCheckpointCandidate::from_json(candidate_json)?;
    if let Some(previous) = input.previous_checkpoint_snapshot() {
        for (_, entry) in previous.sections().iter() {
            if candidate.handoffs().iter().any(|handoff| {
                matches!(handoff, CheckpointHandoff::Keep { old_id } if old_id == entry.id())
            }) {
                metrics.kept_entry_count += 1;
                metrics.kept_entry_tokens += estimate_text_tokens(&entry.render_prompt_text());
            }
        }
        candidate.materialize_kept_entries(previous);
    }
    validate_candidate_uses_model_supplied_refs(&candidate, input)?;
    let policy = CheckpointValidationPolicy::default();
    let checkpoint = match input.previous_checkpoint_snapshot() {
        Some(previous) => CitationBackedCheckpoint::from_rolling_candidate_with_pinned_refs(
            checkpoint_id,
            candidate,
            input.manifest().clone(),
            previous,
            policy,
            input.pinned_refs(),
        ),
        None => CitationBackedCheckpoint::from_candidate_with_pinned_refs(
            checkpoint_id,
            candidate,
            input.manifest().clone(),
            policy,
            input.pinned_refs(),
        ),
    }
    .map_err(RuntimeError::from)?;
    let estimated_tokens = estimate_text_tokens(&checkpoint.render_prompt_text());
    metrics.rendered_summary_tokens = Some(estimated_tokens);
    if estimated_tokens > input.resolved_budget().output_token_limit() {
        return Err(CompactionError::RenderedCheckpointTooLarge {
            estimated_tokens,
            max_tokens: input.resolved_budget().output_token_limit(),
        }
        .into());
    }
    Ok(checkpoint)
}

fn validate_candidate_uses_model_supplied_refs(
    candidate: &CompactedCheckpointCandidate,
    input: &CitationCompactionInput,
) -> Result<(), CheckpointError> {
    for (_, entry) in candidate.sections().iter() {
        for ref_id in entry.refs() {
            if !input.model_supplied_ref_ids().contains(ref_id) {
                return Err(CheckpointError::UnknownRef {
                    entry_id: entry.id().as_str().to_owned(),
                    ref_id: ref_id.as_str().to_owned(),
                });
            }
        }
    }
    Ok(())
}
