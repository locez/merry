//! Compaction history and exact source-reference projection.

use crate::{
    RuntimeError,
    artifact::ArtifactError,
    checkpoint::{
        CheckpointError, CheckpointRef, CheckpointRefId, CheckpointSequenceRange,
        CheckpointSourceKind,
    },
    compaction::{
        CitationCompactionInput, CitationCompactionInputParts, CitationCompactionInputPolicy,
        CitationCompactionModelTurn, CitationCompactionPolicy,
        CitationCompactionPreviousCheckpointInput, CitationCompactionWindowBundle, CompactionError,
        CompactionWindowPlan, ResolvedCitationCompactionBudget, previous_checkpoint_payload,
    },
    session::{
        ModelTurn, ModelTurnId, ModelTurnStatus, PromptHistoryProjection, SessionState, Transcript,
        history::CompactionHistoryItem,
        transcript::{
            ToolCallPromptProjection, ToolResultPromptProjection, TranscriptItem, TranscriptItemId,
        },
    },
};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef, ToolCallId};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HiddenToolExchangeVisibility {
    Include,
    Exclude,
}

pub(super) struct ModelTurnHistory {
    pub(super) id: ModelTurnId,
    pub(super) status: ModelTurnStatus,
    pub(super) items: Vec<CompactionHistoryRecord>,
}

#[derive(Clone)]
pub(super) struct CompactionHistoryRecord {
    pub(super) item: CompactionHistoryItem,
    pub(super) reference: CheckpointRef,
}

impl ModelTurnHistory {
    pub(super) fn projected_token_estimate(
        &self,
        archived_tool_call_ids: &BTreeSet<ToolCallId>,
    ) -> Result<u64, RuntimeError> {
        self.items.iter().try_fold(0_u64, |total, record| {
            total
                .checked_add(
                    record
                        .item
                        .projected_token_estimate(archived_tool_call_ids)?,
                )
                .ok_or_else(|| RuntimeError::from(CompactionError::BudgetOverflow))
        })
    }

    pub(super) fn existing_archived_tool_call_ids(&self) -> BTreeSet<ToolCallId> {
        self.items
            .iter()
            .filter_map(|record| record.item.tool_result_archive_candidate())
            .filter_map(|(_, call_id, already_archived)| already_archived.then_some(call_id))
            .collect()
    }

    pub(super) fn archive_candidates_in_result_order(&self) -> Vec<(u64, ToolCallId)> {
        if self.status != ModelTurnStatus::Completed {
            return Vec::new();
        }
        let mut candidates = self
            .items
            .iter()
            .filter_map(|record| record.item.tool_result_archive_candidate())
            .filter_map(|(result_item_id, call_id, already_archived)| {
                (!already_archived).then_some((result_item_id, call_id))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(result_item_id, _)| *result_item_id);
        candidates
    }
}

impl SessionState {
    pub(super) fn model_turn_histories(
        &self,
        hidden_tool_exchanges: HiddenToolExchangeVisibility,
        apply_prompt_projection: bool,
    ) -> Result<Vec<ModelTurnHistory>, RuntimeError> {
        self.model_turn_histories_for(
            &self.transcript,
            self.prompt_history_projection,
            hidden_tool_exchanges,
            apply_prompt_projection,
        )
    }

    pub(super) fn model_turn_histories_for(
        &self,
        transcript: &Transcript,
        prompt_history_projection: PromptHistoryProjection,
        hidden_tool_exchanges: HiddenToolExchangeVisibility,
        apply_prompt_projection: bool,
    ) -> Result<Vec<ModelTurnHistory>, RuntimeError> {
        let compacted_through = apply_prompt_projection
            .then(|| prompt_history_projection.compacted_through())
            .flatten();
        transcript
            .model_turns()
            .map_err(|_| RuntimeError::from(CompactionError::StaleWindow))?
            .into_iter()
            .filter(|turn| compacted_through.is_none_or(|boundary| turn.id() > boundary))
            .map(|turn| {
                Ok(ModelTurnHistory {
                    id: turn.id(),
                    status: turn.status(),
                    items: self.history_items_for_model_turn(&turn, hidden_tool_exchanges)?,
                })
            })
            .collect()
    }

    pub(super) fn history_items_for_model_turn(
        &self,
        turn: &ModelTurn<'_>,
        hidden_tool_exchanges: HiddenToolExchangeVisibility,
    ) -> Result<Vec<CompactionHistoryRecord>, RuntimeError> {
        let mut items = Vec::with_capacity(turn.items().len());
        let mut results = BTreeMap::new();
        for item in turn.items() {
            if let TranscriptItem::ToolResult {
                id,
                call_id,
                result,
                artifact_id,
                prompt_projection,
                ..
            } = item
                && results
                    .insert(
                        call_id.clone(),
                        (*id, result, artifact_id, *prompt_projection),
                    )
                    .is_some()
            {
                return Err(CompactionError::StaleWindow.into());
            }
        }
        let mut matched_results = BTreeSet::new();

        for item in turn.items() {
            match item {
                TranscriptItem::UserMessage {
                    id, artifact_id, ..
                } => {
                    let content = self.read_artifact_content(artifact_id)?;
                    let text =
                        content
                            .as_text()
                            .ok_or_else(|| ArtifactError::InvalidEvidenceLocator {
                                id: artifact_id.clone(),
                                reason: "user transcript artifact is not textual",
                            })?;
                    items.push(CompactionHistoryRecord {
                        item: CompactionHistoryItem::user(id.as_u64(), text.to_owned()),
                        reference: history_checkpoint_ref(
                            *id,
                            CheckpointSourceKind::UserMessage,
                            artifact_id,
                        )?,
                    });
                }
                TranscriptItem::AssistantText {
                    id, artifact_id, ..
                } => {
                    let content = self.read_artifact_content(artifact_id)?;
                    let text =
                        content
                            .as_text()
                            .ok_or_else(|| ArtifactError::InvalidEvidenceLocator {
                                id: artifact_id.clone(),
                                reason: "assistant transcript artifact is not textual",
                            })?;
                    items.push(CompactionHistoryRecord {
                        item: CompactionHistoryItem::assistant(id.as_u64(), text.to_owned()),
                        reference: history_checkpoint_ref(
                            *id,
                            CheckpointSourceKind::AssistantMessage,
                            artifact_id,
                        )?,
                    });
                }
                TranscriptItem::ToolCall {
                    call,
                    prompt_projection: call_projection,
                    ..
                } => {
                    let Some(&(id, result, artifact_id, result_projection)) =
                        results.get(call.id())
                    else {
                        if turn.status().is_open() {
                            continue;
                        }
                        return Err(CompactionError::StaleWindow.into());
                    };
                    if !matched_results.insert(call.id().clone()) {
                        return Err(CompactionError::StaleWindow.into());
                    }
                    match (*call_projection, result_projection) {
                        (ToolCallPromptProjection::Hidden, ToolResultPromptProjection::Hidden)
                            if hidden_tool_exchanges == HiddenToolExchangeVisibility::Exclude =>
                        {
                            continue;
                        }
                        (ToolCallPromptProjection::Hidden, ToolResultPromptProjection::Hidden)
                        | (ToolCallPromptProjection::Full, ToolResultPromptProjection::Full)
                        | (
                            ToolCallPromptProjection::Full,
                            ToolResultPromptProjection::ArtifactNotice,
                        ) => {}
                        (ToolCallPromptProjection::Hidden, _)
                        | (ToolCallPromptProjection::Full, ToolResultPromptProjection::Hidden) => {
                            return Err(CompactionError::StaleWindow.into());
                        }
                    }
                    let content = self.read_artifact_content(artifact_id)?;
                    items.push(CompactionHistoryRecord {
                        item: CompactionHistoryItem::tool_exchange(
                            id.as_u64(),
                            call.clone(),
                            result.clone(),
                            content,
                            *call_projection,
                            result_projection,
                        ),
                        reference: history_checkpoint_ref(
                            id,
                            CheckpointSourceKind::ToolResult,
                            artifact_id,
                        )?,
                    });
                }
                TranscriptItem::ToolResult { .. } => {}
            }
        }

        if matched_results.len() != results.len() {
            return Err(CompactionError::StaleWindow.into());
        }

        Ok(items)
    }

    pub(super) fn citation_compaction_input_from_history(
        &self,
        policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
        covered: &[&ModelTurnHistory],
        plan: CompactionWindowPlan,
        archived_refs: Vec<CheckpointRef>,
    ) -> Result<CitationCompactionInput, RuntimeError> {
        if covered.iter().all(|turn| turn.items.is_empty()) {
            return Err(CompactionError::NoCompressibleWindow.into());
        }

        let mut covered_history_ids = BTreeSet::new();
        let checkpoint_id = crate::CheckpointId::new(&format!(
            "checkpoint-{}-{}",
            sanitize_checkpoint_component(self.session_id.as_str()),
            self.transcript.next_id().as_u64()
        ))?;
        let previous_checkpoint_input = self.compacted_checkpoint.as_ref().map(|checkpoint| {
            match checkpoint.citation_backed() {
                Some(citation) => {
                    CitationCompactionPreviousCheckpointInput::CitationBacked(citation)
                }
                None => CitationCompactionPreviousCheckpointInput::PlainText {
                    text: checkpoint.text(),
                },
            }
        });
        let prior_refs = match previous_checkpoint_input.as_ref() {
            Some(CitationCompactionPreviousCheckpointInput::CitationBacked(checkpoint)) => {
                checkpoint.manifest().refs().to_vec()
            }
            Some(CitationCompactionPreviousCheckpointInput::PlainText { .. }) | None => Vec::new(),
        };
        let mut refs_by_id = BTreeMap::new();
        for reference in prior_refs {
            refs_by_id.insert(reference.id().clone(), reference);
        }
        let mut window = Vec::with_capacity(covered.len());

        for turn in covered {
            let mut items = Vec::with_capacity(turn.items.len());
            for record in &turn.items {
                covered_history_ids.insert(record.item.history_id);
                items.push(
                    record
                        .item
                        .to_compaction_turn_item(record.reference.id().as_str())?,
                );
                refs_by_id
                    .entry(record.reference.id().clone())
                    .or_insert_with(|| record.reference.clone());
            }
            window.push(CitationCompactionModelTurn::new(
                turn.id,
                turn.status,
                items,
            )?);
        }

        for reference in &archived_refs {
            refs_by_id
                .entry(reference.id().clone())
                .or_insert_with(|| reference.clone());
        }

        let manifest =
            crate::CheckpointRefManifest::new(checkpoint_id, refs_by_id.into_values().collect())?;
        let previous_checkpoint_snapshot = self
            .compacted_checkpoint
            .as_ref()
            .and_then(crate::CompactedCheckpoint::citation_backed)
            .cloned();
        let previous_checkpoint = previous_checkpoint_input.map(previous_checkpoint_payload);

        Ok(CitationCompactionInput::new(
            CitationCompactionInputParts {
                input_policy: CitationCompactionInputPolicy::new(policy, resolved_budget),
                task_anchor_snapshot: self.task_anchor.clone(),
                manifest,
                previous_checkpoint,
                previous_checkpoint_snapshot,
            },
            CitationCompactionWindowBundle {
                covered_history_ids,
                window,
                window_plan: plan,
                archived_refs,
            },
        ))
    }

    pub(super) fn provider_history_item_count_for(
        &self,
        transcript: &Transcript,
        prompt_history_projection: PromptHistoryProjection,
    ) -> Result<usize, RuntimeError> {
        Ok(self
            .model_turn_histories_for(
                transcript,
                prompt_history_projection,
                HiddenToolExchangeVisibility::Exclude,
                true,
            )?
            .into_iter()
            .map(|turn| turn.items.len())
            .sum())
    }
}

pub(super) fn history_checkpoint_ref(
    item_id: TranscriptItemId,
    source_kind: CheckpointSourceKind,
    artifact_id: &ArtifactId,
) -> Result<CheckpointRef, CheckpointError> {
    Ok(CheckpointRef::new(
        history_ref_id(item_id)?,
        source_kind,
        CheckpointSequenceRange::new(item_id.as_u64(), item_id.as_u64())?,
        EvidenceRef::new(artifact_id.clone(), EvidenceLocator::whole_artifact()),
    ))
}

pub(super) fn history_ref_id(
    item_id: TranscriptItemId,
) -> Result<CheckpointRefId, CheckpointError> {
    CheckpointRefId::new(&format!("h{}", item_id.as_u64()))
}

pub(super) fn sanitize_checkpoint_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ':') {
                character
            } else {
                '_'
            }
        })
        .collect()
}
