use super::{
    SessionState, history::CompactionHistoryItem, history::permission_review_context_entry,
    transcript::TranscriptItem,
};
use crate::{
    RuntimeError,
    artifact::ArtifactError,
    checkpoint::{CheckpointError, CheckpointId, CheckpointRefExcerpt, CheckpointRefId},
    compaction::{
        CitationCompactionInput, CitationCompactionPolicy,
        CitationCompactionPreviousCheckpointInput, CompactionError, CompactionOutcome,
        checkpoint_from_candidate_json, previous_checkpoint_payload,
    },
    context::{CompactedCheckpoint, CompactedCheckpointSummary},
    permission::PermissionReviewContextEntry,
};
use std::collections::BTreeSet;

const PERMISSION_REVIEW_RECENT_CONTEXT_LIMIT: usize = 12;

impl SessionState {
    pub(crate) fn set_compacted_checkpoint(&mut self, checkpoint: CompactedCheckpoint) {
        self.compacted_checkpoint = Some(checkpoint);
    }

    pub(crate) fn compacted_checkpoint_summary(&self) -> Option<CompactedCheckpointSummary> {
        self.compacted_checkpoint
            .as_ref()
            .map(CompactedCheckpoint::summary)
    }

    pub(crate) fn read_checkpoint_ref(
        &self,
        checkpoint_id: &CheckpointId,
        ref_id: &CheckpointRefId,
    ) -> Result<CheckpointRefExcerpt, CheckpointError> {
        let Some(checkpoint) = &self.compacted_checkpoint else {
            return Err(CheckpointError::RefNotFound {
                checkpoint_id: checkpoint_id.as_str().to_owned(),
                ref_id: ref_id.as_str().to_owned(),
            });
        };

        checkpoint.read_checkpoint_ref(checkpoint_id, ref_id)
    }

    pub(crate) fn build_citation_compaction_input(
        &self,
        policy: CitationCompactionPolicy,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(CompactionError::PendingToolCalls.into());
        }

        let history = self.compaction_history_items()?;
        if history.len() <= policy.retained_raw_tail_items() {
            return Ok(None);
        }

        let covered_count = history.len() - policy.retained_raw_tail_items();
        let covered = &history[..covered_count];
        self.citation_compaction_input_from_history(policy, covered)
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
        self.validate_compaction_window_is_current(&input)?;

        let checkpoint_id = input.manifest().checkpoint_id().clone();
        let citation =
            checkpoint_from_candidate_json(checkpoint_id.clone(), &input, candidate_json)?;
        let compacted = CompactedCheckpoint::from_citation_backed(citation)?;

        let covered_count = input.covered_history_ids().len();
        self.transcript
            .remove_compacted_history_prefix(covered_count);
        self.compacted_checkpoint = Some(compacted);

        Ok(CompactionOutcome::new(
            checkpoint_id,
            covered_count,
            self.history_item_count()?,
        ))
    }

    pub(crate) fn permission_review_context_snapshot(
        &self,
    ) -> Result<Vec<PermissionReviewContextEntry>, RuntimeError> {
        let items = self.compaction_history_items()?;
        let start = items
            .len()
            .saturating_sub(PERMISSION_REVIEW_RECENT_CONTEXT_LIMIT);
        Ok(items[start..]
            .iter()
            .map(permission_review_context_entry)
            .collect())
    }

    fn compaction_history_items(&self) -> Result<Vec<CompactionHistoryItem>, RuntimeError> {
        let mut items = Vec::with_capacity(self.transcript.items().len());
        let transcript_items = self.transcript.items();
        let mut index = 0usize;

        while index < transcript_items.len() {
            match &transcript_items[index] {
                TranscriptItem::UserMessage { id, text, .. } => {
                    items.push(CompactionHistoryItem::user(id.as_u64(), text.clone()));
                    index += 1;
                }
                TranscriptItem::AssistantText { id, artifact_id } => {
                    let content = self.read_artifact_content(artifact_id)?;
                    let text =
                        content
                            .as_text()
                            .ok_or_else(|| ArtifactError::InvalidEvidenceLocator {
                                id: artifact_id.clone(),
                                reason: "assistant transcript artifact is not textual",
                            })?;
                    items.push(CompactionHistoryItem::assistant(
                        id.as_u64(),
                        text.to_owned(),
                    ));
                    index += 1;
                }
                TranscriptItem::ToolCall { call, .. } => {
                    let Some(next_item) = transcript_items.get(index + 1) else {
                        if self
                            .pending_tool_calls
                            .iter()
                            .any(|pending| pending.id() == call.id())
                        {
                            index += 1;
                            continue;
                        }
                        return Err(CompactionError::StaleWindow.into());
                    };
                    let TranscriptItem::ToolResult {
                        id,
                        call_id,
                        result,
                        artifact_id,
                    } = next_item
                    else {
                        if self
                            .pending_tool_calls
                            .iter()
                            .any(|pending| pending.id() == call.id())
                        {
                            index += 1;
                            continue;
                        }
                        return Err(CompactionError::StaleWindow.into());
                    };
                    if call.id() != call_id {
                        return Err(CompactionError::StaleWindow.into());
                    }
                    let content = self.read_artifact_content(artifact_id)?;
                    items.push(CompactionHistoryItem::tool_exchange(
                        id.as_u64(),
                        call.clone(),
                        result.clone(),
                        content,
                    ));
                    index += 2;
                }
                TranscriptItem::ToolResult { .. } => {
                    return Err(CompactionError::StaleWindow.into());
                }
            }
        }

        Ok(items)
    }

    fn citation_compaction_input_from_history(
        &self,
        policy: CitationCompactionPolicy,
        covered: &[CompactionHistoryItem],
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
        let previous_checkpoint = self.compacted_checkpoint.as_ref().map(|checkpoint| {
            match checkpoint.citation_backed() {
                Some(citation) => {
                    CitationCompactionPreviousCheckpointInput::CitationBacked(citation)
                }
                None => CitationCompactionPreviousCheckpointInput::PlainText {
                    text: checkpoint.text(),
                },
            }
        });
        let prior_refs = match previous_checkpoint {
            Some(CitationCompactionPreviousCheckpointInput::CitationBacked(checkpoint)) => {
                crate::CheckpointRefManifest::from_prior_checkpoint_claims(
                    checkpoint_id.clone(),
                    checkpoint,
                    policy.max_carried_prior_refs(),
                )?
                .refs()
                .to_vec()
            }
            Some(CitationCompactionPreviousCheckpointInput::PlainText { .. }) | None => Vec::new(),
        };
        let mut refs = Vec::with_capacity(prior_refs.len() + covered.len());
        refs.extend(prior_refs);
        let mut window = Vec::with_capacity(covered.len());

        for (index, item) in covered.iter().enumerate() {
            covered_history_ids.insert(item.history_id);
            let ref_id = format!("r{}", index + 1);
            let window_item = item.to_compaction_window_item(&ref_id, policy)?;
            refs.push(window_item.to_checkpoint_ref(policy.max_ref_excerpt_bytes())?);
            window.push(window_item);
        }

        let manifest = crate::CheckpointRefManifest::new(checkpoint_id, refs)?;
        let previous_checkpoint = previous_checkpoint.map(|checkpoint| {
            previous_checkpoint_payload(checkpoint, policy.max_carried_prior_refs())
        });

        Ok(CitationCompactionInput::new(
            policy,
            self.task_anchor.clone(),
            manifest,
            covered_history_ids,
            window,
            previous_checkpoint,
        ))
    }

    fn validate_compaction_window_is_current(
        &self,
        input: &CitationCompactionInput,
    ) -> Result<(), RuntimeError> {
        let current_ids = self.current_history_id_set()?;
        let covered = input.covered_history_ids();
        let Some(max_covered_id) = covered.iter().next_back().copied() else {
            return Err(CompactionError::NoCompressibleWindow.into());
        };

        let current_prefix = current_ids
            .into_iter()
            .take_while(|history_id| *history_id <= max_covered_id)
            .collect::<BTreeSet<_>>();
        if &current_prefix != covered {
            return Err(CompactionError::StaleWindow.into());
        }

        Ok(())
    }

    fn current_history_id_set(&self) -> Result<BTreeSet<u64>, RuntimeError> {
        Ok(self
            .compaction_history_items()?
            .into_iter()
            .map(|item| item.history_id)
            .collect())
    }

    fn history_item_count(&self) -> Result<usize, RuntimeError> {
        Ok(self.compaction_history_items()?.len())
    }
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
