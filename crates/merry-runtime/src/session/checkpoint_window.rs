use super::{
    ModelTurn, ModelTurnId, ModelTurnStatus, SessionState,
    history::CompactionHistoryItem,
    history::permission_review_context_entry,
    transcript::{
        ToolCallPromptProjection, ToolResultPromptProjection, TranscriptItem, TranscriptItemId,
    },
};
use crate::{
    RuntimeError,
    artifact::{ArtifactError, TextEvidencePage},
    checkpoint::{
        CheckpointError, CheckpointRef, CheckpointRefId, CheckpointSequenceRange,
        CheckpointSourceKind,
    },
    compaction::{
        CitationCompactionInput, CitationCompactionPolicy,
        CitationCompactionPreviousCheckpointInput, CompactionError, CompactionOutcome,
        checkpoint_from_candidate_json, previous_checkpoint_payload,
    },
    context::{CompactedCheckpoint, CompactedCheckpointSummary},
    permission::PermissionReviewContextEntry,
};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};
use std::collections::{BTreeMap, BTreeSet};

const PERMISSION_REVIEW_RECENT_CONTEXT_LIMIT: usize = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
enum HiddenToolExchangeVisibility {
    Include,
    Exclude,
}

struct ModelTurnHistory {
    id: ModelTurnId,
    status: ModelTurnStatus,
    items: Vec<CompactionHistoryRecord>,
}

#[derive(Clone)]
struct CompactionHistoryRecord {
    item: CompactionHistoryItem,
    reference: CheckpointRef,
}

impl SessionState {
    pub(crate) fn set_compacted_checkpoint(&mut self, checkpoint: CompactedCheckpoint) {
        self.compacted_checkpoint = Some(checkpoint);
    }

    pub(crate) fn compacted_checkpoint_summary(&self) -> Option<CompactedCheckpointSummary> {
        self.compacted_checkpoint
            .as_ref()
            .map(CompactedCheckpoint::summary)
    }

    pub(crate) fn validate_compacted_checkpoint_evidence(
        &self,
        checkpoint: &CompactedCheckpoint,
    ) -> Result<(), ArtifactError> {
        let Some(checkpoint) = checkpoint.citation_backed() else {
            return Ok(());
        };
        for reference in checkpoint.manifest().refs() {
            self.artifacts
                .validate_text_evidence(reference.evidence())?;
        }
        Ok(())
    }

    pub(crate) fn read_checkpoint_ref_page(
        &self,
        ref_id: &CheckpointRefId,
        offset: usize,
        max_bytes: usize,
    ) -> Result<TextEvidencePage, RuntimeError> {
        self.read_checkpoint_ref_page_with_source(ref_id, offset, max_bytes)
            .map(|(_, page)| page)
    }

    pub(crate) fn read_checkpoint_ref_page_with_source(
        &self,
        ref_id: &CheckpointRefId,
        offset: usize,
        max_bytes: usize,
    ) -> Result<(CheckpointSourceKind, TextEvidencePage), RuntimeError> {
        let Some(checkpoint) = self
            .compacted_checkpoint
            .as_ref()
            .and_then(CompactedCheckpoint::citation_backed)
        else {
            return Err(CheckpointError::RefNotFound {
                checkpoint_id: "current".to_owned(),
                ref_id: ref_id.as_str().to_owned(),
            }
            .into());
        };
        let reference = checkpoint.read_ref(ref_id)?;
        let page =
            self.artifacts
                .read_text_evidence_page(reference.evidence(), offset, max_bytes)?;
        Ok((reference.source_kind(), page))
    }

    pub(crate) fn build_citation_compaction_input(
        &self,
        policy: CitationCompactionPolicy,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(CompactionError::PendingToolCalls.into());
        }

        let turns = self.model_turn_histories(HiddenToolExchangeVisibility::Include, true)?;
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
        let completed_turns = &turns[..first_open];
        let history_count = completed_turns
            .iter()
            .map(|turn| turn.items.len())
            .sum::<usize>();
        if history_count <= policy.retained_raw_tail_items() {
            return Ok(None);
        }

        let mut retained_count = 0;
        let mut covered_turn_count = completed_turns.len();
        while covered_turn_count > 0 && retained_count < policy.retained_raw_tail_items() {
            covered_turn_count -= 1;
            retained_count += completed_turns[covered_turn_count].items.len();
        }
        let covered = completed_turns[..covered_turn_count]
            .iter()
            .flat_map(|turn| turn.items.iter().cloned())
            .collect::<Vec<_>>();
        if covered.is_empty() {
            return Ok(None);
        }
        self.citation_compaction_input_from_history(policy, &covered)
            .map(Some)
    }

    pub(crate) fn install_citation_compaction_candidate(
        &mut self,
        input: CitationCompactionInput,
        candidate_json: &str,
    ) -> Result<CompactionOutcome, RuntimeError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(CompactionError::PendingToolCalls.into());
        }
        let compacted_through = self.validate_compaction_window_is_current(&input)?;

        let checkpoint_id = input.manifest().checkpoint_id().clone();
        let citation =
            checkpoint_from_candidate_json(checkpoint_id.clone(), &input, candidate_json)?;
        let compacted = CompactedCheckpoint::from_citation_backed(citation)?;

        let covered_count = input.covered_history_ids().len();
        self.advance_prompt_history_projection(compacted_through)?;
        self.compacted_checkpoint = Some(compacted);

        Ok(CompactionOutcome::new(
            checkpoint_id,
            covered_count,
            self.provider_history_item_count()?,
        ))
    }

    pub(crate) fn permission_review_context_snapshot(
        &self,
    ) -> Result<Vec<PermissionReviewContextEntry>, RuntimeError> {
        let items = self
            .model_turn_histories(HiddenToolExchangeVisibility::Include, true)?
            .into_iter()
            .flat_map(|turn| turn.items)
            .collect::<Vec<_>>();
        let start = items
            .len()
            .saturating_sub(PERMISSION_REVIEW_RECENT_CONTEXT_LIMIT);
        Ok(items[start..]
            .iter()
            .map(|record| permission_review_context_entry(&record.item))
            .collect())
    }

    fn model_turn_histories(
        &self,
        hidden_tool_exchanges: HiddenToolExchangeVisibility,
        apply_prompt_projection: bool,
    ) -> Result<Vec<ModelTurnHistory>, RuntimeError> {
        let compacted_through = apply_prompt_projection
            .then(|| self.prompt_history_projection().compacted_through())
            .flatten();
        self.transcript
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

    fn history_items_for_model_turn(
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

    fn citation_compaction_input_from_history(
        &self,
        policy: CitationCompactionPolicy,
        covered: &[CompactionHistoryRecord],
    ) -> Result<CitationCompactionInput, RuntimeError> {
        if covered.is_empty() {
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
        let mut refs = Vec::with_capacity(prior_refs.len() + covered.len());
        refs.extend(prior_refs);
        let mut window = Vec::with_capacity(covered.len());

        for record in covered {
            covered_history_ids.insert(record.item.history_id);
            let window_item = record
                .item
                .to_compaction_window_item(record.reference.id().as_str(), policy)?;
            refs.push(record.reference.clone());
            window.push(window_item);
        }

        let manifest = crate::CheckpointRefManifest::new(checkpoint_id, refs)?;
        let previous_checkpoint_snapshot = self
            .compacted_checkpoint
            .as_ref()
            .and_then(crate::CompactedCheckpoint::citation_backed)
            .cloned();
        let previous_checkpoint = previous_checkpoint_input.map(previous_checkpoint_payload);

        Ok(CitationCompactionInput::new(
            policy,
            self.task_anchor.clone(),
            manifest,
            covered_history_ids,
            window,
            previous_checkpoint,
            previous_checkpoint_snapshot,
        ))
    }

    fn validate_compaction_window_is_current(
        &self,
        input: &CitationCompactionInput,
    ) -> Result<ModelTurnId, RuntimeError> {
        let covered = input.covered_history_ids();
        if covered.is_empty() {
            return Err(CompactionError::NoCompressibleWindow.into());
        }

        let turns = self.model_turn_histories(HiddenToolExchangeVisibility::Include, true)?;
        let mut current_prefix = BTreeSet::new();
        let mut matched_boundary = None;
        for turn in turns {
            if turn.status.is_open() {
                break;
            }
            for item in &turn.items {
                if !covered.contains(&item.item.history_id) {
                    return matched_boundary
                        .ok_or_else(|| RuntimeError::from(CompactionError::StaleWindow));
                }
                current_prefix.insert(item.item.history_id);
            }
            if &current_prefix == covered {
                matched_boundary = Some(turn.id);
            }
        }
        matched_boundary.ok_or_else(|| RuntimeError::from(CompactionError::StaleWindow))
    }

    fn provider_history_item_count(&self) -> Result<usize, RuntimeError> {
        Ok(self
            .model_turn_histories(HiddenToolExchangeVisibility::Exclude, true)?
            .into_iter()
            .map(|turn| turn.items.len())
            .sum())
    }
}

fn history_checkpoint_ref(
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

fn history_ref_id(item_id: TranscriptItemId) -> Result<CheckpointRefId, CheckpointError> {
    CheckpointRefId::new(&format!("h{}", item_id.as_u64()))
}

fn sanitize_checkpoint_component(value: &str) -> String {
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
