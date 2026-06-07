use super::{
    SessionMessage, SessionState, history::CompactionHistoryItem,
    history::permission_review_context_entry,
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

        let covered = input.covered_history_ids().clone();
        let covered_count = covered.len();
        self.append_only_body
            .retain(|message| !covered.contains(&message.history_id()));
        self.uncheckpointed_tool_continuations
            .retain(|continuation| !covered.contains(&continuation.history_id));
        self.compacted_checkpoint = Some(compacted);

        Ok(CompactionOutcome::new(
            checkpoint_id,
            covered_count,
            self.history_item_count(),
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
        let mut items = Vec::with_capacity(
            self.append_only_body.len() + self.uncheckpointed_tool_continuations.len(),
        );

        for message in &self.append_only_body {
            match message {
                SessionMessage::User { history_id, text } => {
                    items.push(CompactionHistoryItem::user(*history_id, text.clone()));
                }
                SessionMessage::Assistant {
                    history_id,
                    artifact_id,
                } => {
                    let content = self.read_artifact_content(artifact_id)?;
                    let text =
                        content
                            .as_text()
                            .ok_or_else(|| ArtifactError::InvalidEvidenceLocator {
                                id: artifact_id.clone(),
                                reason: "assistant history artifact is not textual",
                            })?;
                    items.push(CompactionHistoryItem::assistant(
                        *history_id,
                        text.to_owned(),
                    ));
                }
            }
        }

        for continuation in &self.uncheckpointed_tool_continuations {
            let content = self
                .artifacts
                .read_content(continuation.result.artifact().id())?
                .clone();
            items.push(CompactionHistoryItem::tool_exchange(
                continuation.history_id,
                continuation.call.clone(),
                continuation.result.clone(),
                content,
            ));
        }

        items.sort_by_key(|item| item.history_id);
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
            self.next_history_id
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
        let current_ids = self.current_history_id_set();
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

    fn current_history_id_set(&self) -> BTreeSet<u64> {
        self.append_only_body
            .iter()
            .map(SessionMessage::history_id)
            .chain(
                self.uncheckpointed_tool_continuations
                    .iter()
                    .map(|continuation| continuation.history_id),
            )
            .collect()
    }

    fn history_item_count(&self) -> usize {
        self.append_only_body.len() + self.uncheckpointed_tool_continuations.len()
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
