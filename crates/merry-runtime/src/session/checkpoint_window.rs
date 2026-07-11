use super::{
    SessionState,
    history::CompactionHistoryItem,
    history::permission_review_context_entry,
    transcript::{ToolCallPromptProjection, ToolResultPromptProjection, TranscriptItem},
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
use std::collections::{BTreeMap, BTreeSet};

const PERMISSION_REVIEW_RECENT_CONTEXT_LIMIT: usize = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
enum HiddenToolExchangeVisibility {
    Include,
    Exclude,
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

        let history = self.history_items(HiddenToolExchangeVisibility::Exclude)?;
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

        let covered_history_ids = input.covered_history_ids().clone();
        let covered_count = covered_history_ids.len();
        self.transcript
            .remove_compacted_history(&covered_history_ids);
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
        let items = self.history_items(HiddenToolExchangeVisibility::Include)?;
        let start = items
            .len()
            .saturating_sub(PERMISSION_REVIEW_RECENT_CONTEXT_LIMIT);
        Ok(items[start..]
            .iter()
            .map(permission_review_context_entry)
            .collect())
    }

    fn history_items(
        &self,
        hidden_tool_exchanges: HiddenToolExchangeVisibility,
    ) -> Result<Vec<CompactionHistoryItem>, RuntimeError> {
        let mut items = Vec::with_capacity(self.transcript.items().len());
        let transcript_items = self.transcript.items();
        let mut results = BTreeMap::new();
        for item in transcript_items {
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

        for item in transcript_items {
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
                    items.push(CompactionHistoryItem::user(id.as_u64(), text.to_owned()));
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
                    items.push(CompactionHistoryItem::assistant(
                        id.as_u64(),
                        text.to_owned(),
                    ));
                }
                TranscriptItem::ToolCall {
                    call,
                    prompt_projection: call_projection,
                    ..
                } => {
                    let Some(&(id, result, artifact_id, result_projection)) =
                        results.get(call.id())
                    else {
                        if self
                            .pending_tool_calls
                            .iter()
                            .any(|pending| pending.id() == call.id())
                        {
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
                    items.push(CompactionHistoryItem::tool_exchange(
                        id.as_u64(),
                        call.clone(),
                        result.clone(),
                        content,
                    ));
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
        let current_ids = self
            .history_items(HiddenToolExchangeVisibility::Exclude)?
            .into_iter()
            .map(|item| item.history_id)
            .collect::<Vec<_>>();
        let covered = input.covered_history_ids();
        if covered.is_empty() {
            return Err(CompactionError::NoCompressibleWindow.into());
        }

        let current_prefix = current_ids
            .into_iter()
            .take(covered.len())
            .collect::<BTreeSet<_>>();
        if &current_prefix != covered {
            return Err(CompactionError::StaleWindow.into());
        }

        Ok(())
    }

    fn history_item_count(&self) -> Result<usize, RuntimeError> {
        Ok(self
            .history_items(HiddenToolExchangeVisibility::Exclude)?
            .len())
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
