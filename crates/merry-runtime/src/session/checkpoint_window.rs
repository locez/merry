use super::{
    ModelTurn, ModelTurnId, ModelTurnStatus, PromptHistoryProjection, SessionState, Transcript,
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
        ArchiveOnlyCompactionInput, CitationCompactionInput, CitationCompactionInputParts,
        CitationCompactionInputPolicy, CitationCompactionModelTurn, CitationCompactionPolicy,
        CitationCompactionPreviousCheckpointInput, CitationCompactionWindowBundle, CompactionError,
        CompactionOutcome, CompactionPreparation, CompactionWindowBudget,
        CompactionWindowFingerprint, CompactionWindowPlan, ResolvedCitationCompactionBudget,
        checkpoint_from_candidate_json, previous_checkpoint_payload, retained_turn_fallbacks,
    },
    context::{CompactedCheckpoint, CompactedCheckpointSummary},
    permission::PermissionReviewContextEntry,
};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef, ToolCallId};
use std::collections::{BTreeMap, BTreeSet};

const PERMISSION_REVIEW_RECENT_CONTEXT_LIMIT: usize = 12;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ArchivedRefManifest {
    refs: Vec<CheckpointRef>,
}

impl ArchivedRefManifest {
    pub(crate) fn new(refs: Vec<CheckpointRef>) -> Result<Self, CheckpointError> {
        let mut seen = BTreeSet::new();
        for reference in &refs {
            if !seen.insert(reference.id().clone()) {
                return Err(CheckpointError::DuplicateRef {
                    ref_id: reference.id().as_str().to_owned(),
                });
            }
        }
        Ok(Self { refs })
    }

    pub(crate) fn refs(&self) -> &[CheckpointRef] {
        &self.refs
    }

    fn get(&self, ref_id: &CheckpointRefId) -> Option<&CheckpointRef> {
        self.refs.iter().find(|reference| reference.id() == ref_id)
    }

    fn fingerprint_material(&self) -> Vec<(String, CheckpointSourceKind, u64, u64, EvidenceRef)> {
        self.refs
            .iter()
            .map(|reference| {
                (
                    reference.id().as_str().to_owned(),
                    reference.source_kind(),
                    reference.sequence_range().start(),
                    reference.sequence_range().end(),
                    reference.evidence().clone(),
                )
            })
            .collect()
    }
}

#[derive(Debug)]
#[allow(private_interfaces)]
pub(crate) enum PreparedCompactionInstall {
    ReplaceCheckpoint {
        state: PreparedCompactionState,
        outcome: CompactionOutcome,
    },
    ArchiveOnly {
        state: PreparedCompactionState,
    },
}

#[derive(Debug)]
struct PreparedCompactionState {
    transcript: Transcript,
    prompt_history_projection: PromptHistoryProjection,
    compacted_checkpoint: Option<CompactedCheckpoint>,
    archived_ref_manifest: ArchivedRefManifest,
    original_fingerprint: CompactionWindowFingerprint,
}

impl PreparedCompactionInstall {
    fn state(&self) -> &PreparedCompactionState {
        match self {
            Self::ReplaceCheckpoint { state, .. } | Self::ArchiveOnly { state } => state,
        }
    }

    fn into_parts(self) -> (PreparedCompactionState, Option<CompactionOutcome>) {
        match self {
            Self::ReplaceCheckpoint { state, outcome } => (state, Some(outcome)),
            Self::ArchiveOnly { state } => (state, None),
        }
    }

    pub(crate) fn transcript(&self) -> &Transcript {
        &self.state().transcript
    }

    pub(crate) fn prompt_history_projection(&self) -> PromptHistoryProjection {
        self.state().prompt_history_projection
    }

    pub(crate) fn compacted_checkpoint(&self) -> Option<&CompactedCheckpoint> {
        self.state().compacted_checkpoint.as_ref()
    }

    pub(crate) fn archived_ref_manifest(&self) -> &ArchivedRefManifest {
        &self.state().archived_ref_manifest
    }

    pub(crate) fn original_fingerprint(&self) -> CompactionWindowFingerprint {
        self.state().original_fingerprint
    }

    #[cfg(test)]
    pub(crate) fn outcome(&self) -> Option<&CompactionOutcome> {
        match self {
            Self::ReplaceCheckpoint { outcome, .. } => Some(outcome),
            Self::ArchiveOnly { .. } => None,
        }
    }
}

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

impl ModelTurnHistory {
    fn projected_token_estimate(
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

    fn existing_archived_tool_call_ids(&self) -> BTreeSet<ToolCallId> {
        self.items
            .iter()
            .filter_map(|record| record.item.tool_result_archive_candidate())
            .filter_map(|(_, call_id, already_archived)| already_archived.then_some(call_id))
            .collect()
    }

    fn archive_candidates_in_result_order(&self) -> Vec<(u64, ToolCallId)> {
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
        let checkpoint_reference = self
            .compacted_checkpoint
            .as_ref()
            .and_then(CompactedCheckpoint::citation_backed)
            .and_then(|checkpoint| {
                checkpoint
                    .manifest()
                    .refs()
                    .iter()
                    .find(|reference| reference.id() == ref_id)
            });
        let Some(reference) =
            checkpoint_reference.or_else(|| self.archived_ref_manifest.get(ref_id))
        else {
            return Err(CheckpointError::RefNotFound {
                checkpoint_id: "current-or-archive".to_owned(),
                ref_id: ref_id.as_str().to_owned(),
            }
            .into());
        };
        let page =
            self.artifacts
                .read_text_evidence_page(reference.evidence(), offset, max_bytes)?;
        Ok((reference.source_kind(), page))
    }

    pub(crate) fn build_citation_compaction_input(
        &self,
        policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError> {
        let window_budget = CompactionWindowBudget::unbounded_for_manual_compaction(
            resolved_budget.output_token_limit(),
        )?;
        self.build_citation_compaction_input_with_window_budget(
            policy,
            resolved_budget,
            window_budget,
        )
    }

    pub(crate) fn build_citation_compaction_input_with_window_budget(
        &self,
        policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
        window_budget: CompactionWindowBudget,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError> {
        match self.build_compaction_preparation_with_window_budget(
            policy,
            resolved_budget,
            window_budget,
        )? {
            Some(CompactionPreparation::ReplaceCheckpoint(input)) => Ok(Some(*input)),
            Some(CompactionPreparation::ArchiveToolResults(_)) | None => Ok(None),
        }
    }

    pub(crate) fn build_compaction_preparation_with_window_budget(
        &self,
        policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
        window_budget: CompactionWindowBudget,
    ) -> Result<Option<CompactionPreparation>, RuntimeError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(CompactionError::PendingToolCalls.into());
        }

        let turns = self.model_turn_histories(HiddenToolExchangeVisibility::Include, true)?;
        let Some(plan) = self.plan_compaction_window_from_turns(policy, window_budget, &turns)?
        else {
            return Ok(None);
        };
        let archived_refs = archived_refs_for_plan(&turns, &plan)?;
        if plan.covered_turn_ids().is_empty() {
            return Ok(Some(CompactionPreparation::ArchiveToolResults(
                ArchiveOnlyCompactionInput::new(plan, archived_refs),
            )));
        }
        let covered_turn_ids = plan
            .covered_turn_ids()
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let covered = turns
            .iter()
            .filter(|turn| covered_turn_ids.contains(&turn.id))
            .collect::<Vec<_>>();
        self.citation_compaction_input_from_history(
            policy,
            resolved_budget,
            &covered,
            plan,
            archived_refs,
        )
        .map(Box::new)
        .map(CompactionPreparation::ReplaceCheckpoint)
        .map(Some)
    }

    #[cfg(test)]
    pub(crate) fn plan_compaction_window(
        &self,
        policy: CitationCompactionPolicy,
        window_budget: CompactionWindowBudget,
    ) -> Result<Option<CompactionWindowPlan>, RuntimeError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(CompactionError::PendingToolCalls.into());
        }
        let turns = self.model_turn_histories(HiddenToolExchangeVisibility::Include, true)?;
        self.plan_compaction_window_from_turns(policy, window_budget, &turns)
    }

    #[cfg(test)]
    pub(crate) fn install_citation_compaction_candidate(
        &mut self,
        input: CitationCompactionInput,
        candidate_json: &str,
    ) -> Result<CompactionOutcome, RuntimeError> {
        let prepared = self.prepare_citation_compaction_install(input, candidate_json)?;
        self.revalidate_prepared_compaction_install(&prepared)?;
        Ok(self
            .commit_prepared_compaction_install(prepared)
            .expect("prepared checkpoint replacement must carry an outcome"))
    }

    pub(crate) fn prepare_citation_compaction_install(
        &self,
        input: CitationCompactionInput,
        candidate_json: &str,
    ) -> Result<PreparedCompactionInstall, RuntimeError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(CompactionError::PendingToolCalls.into());
        }
        let compacted_through = self.validate_compaction_window_is_current(&input)?;
        let original_fingerprint = input.window_plan().fingerprint();
        let next_archive_manifest = ArchivedRefManifest::new(input.archived_refs().to_vec())?;

        let checkpoint_id = input.manifest().checkpoint_id().clone();
        let citation =
            checkpoint_from_candidate_json(checkpoint_id.clone(), &input, candidate_json)?;
        let compacted = CompactedCheckpoint::from_citation_backed(citation)?;

        let covered_count = input.covered_history_ids().len();
        let mut transcript = self.transcript.clone();
        transcript.archive_tool_results(input.window_plan().archived_tool_call_ids())?;
        let prompt_history_projection = self
            .prompt_history_projection
            .advanced_through(&transcript, compacted_through)?;
        let compacted_checkpoint = Some(compacted);
        prompt_history_projection.validate(&transcript, compacted_checkpoint.as_ref())?;
        self.validate_archived_ref_manifest_for(
            &transcript,
            prompt_history_projection,
            &next_archive_manifest,
        )?;
        let outcome = CompactionOutcome::new(
            checkpoint_id,
            covered_count,
            self.provider_history_item_count_for(&transcript, prompt_history_projection)?,
        );

        Ok(PreparedCompactionInstall::ReplaceCheckpoint {
            state: PreparedCompactionState {
                transcript,
                prompt_history_projection,
                compacted_checkpoint,
                archived_ref_manifest: next_archive_manifest,
                original_fingerprint,
            },
            outcome,
        })
    }

    #[cfg(test)]
    pub(crate) fn install_archive_only_compaction(
        &mut self,
        input: ArchiveOnlyCompactionInput,
    ) -> Result<(), RuntimeError> {
        let prepared = self.prepare_archive_only_compaction_install(input)?;
        self.revalidate_prepared_compaction_install(&prepared)?;
        let outcome = self.commit_prepared_compaction_install(prepared);
        debug_assert!(
            outcome.is_none(),
            "prepared archive-only install must not carry an outcome"
        );
        Ok(())
    }

    pub(crate) fn prepare_archive_only_compaction_install(
        &self,
        input: ArchiveOnlyCompactionInput,
    ) -> Result<PreparedCompactionInstall, RuntimeError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(CompactionError::PendingToolCalls.into());
        }
        if !input.window_plan().covered_turn_ids().is_empty()
            || input.window_plan().new_boundary().is_some()
        {
            return Err(CompactionError::StaleWindow.into());
        }
        self.validate_window_plan_is_current(input.window_plan())?;
        let current_refs = archived_refs_for_plan(
            &self.model_turn_histories(HiddenToolExchangeVisibility::Include, true)?,
            input.window_plan(),
        )?;
        if current_refs != input.archived_refs() {
            return Err(CompactionError::StaleWindow.into());
        }
        let original_fingerprint = input.window_plan().fingerprint();
        let next_archive_manifest = ArchivedRefManifest::new(input.archived_refs().to_vec())?;

        let mut transcript = self.transcript.clone();
        transcript.archive_tool_results(input.window_plan().archived_tool_call_ids())?;
        let prompt_history_projection = self.prompt_history_projection;
        let compacted_checkpoint = self.compacted_checkpoint.clone();
        prompt_history_projection.validate(&transcript, compacted_checkpoint.as_ref())?;
        self.validate_archived_ref_manifest_for(
            &transcript,
            prompt_history_projection,
            &next_archive_manifest,
        )?;

        Ok(PreparedCompactionInstall::ArchiveOnly {
            state: PreparedCompactionState {
                transcript,
                prompt_history_projection,
                compacted_checkpoint,
                archived_ref_manifest: next_archive_manifest,
                original_fingerprint,
            },
        })
    }

    pub(crate) fn revalidate_prepared_compaction_install(
        &self,
        prepared: &PreparedCompactionInstall,
    ) -> Result<(), RuntimeError> {
        if !self.pending_tool_calls.is_empty()
            || self.compaction_window_fingerprint()? != prepared.original_fingerprint()
        {
            return Err(CompactionError::StaleWindow.into());
        }
        Ok(())
    }

    pub(crate) fn commit_prepared_compaction_install(
        &mut self,
        prepared: PreparedCompactionInstall,
    ) -> Option<CompactionOutcome> {
        let (state, outcome) = prepared.into_parts();
        self.transcript = state.transcript;
        self.prompt_history_projection = state.prompt_history_projection;
        self.compacted_checkpoint = state.compacted_checkpoint;
        self.archived_ref_manifest = state.archived_ref_manifest;
        outcome
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
        self.model_turn_histories_for(
            &self.transcript,
            self.prompt_history_projection,
            hidden_tool_exchanges,
            apply_prompt_projection,
        )
    }

    fn model_turn_histories_for(
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

    fn plan_compaction_window_from_turns(
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

    fn citation_compaction_input_from_history(
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

    fn validate_compaction_window_is_current(
        &self,
        input: &CitationCompactionInput,
    ) -> Result<ModelTurnId, RuntimeError> {
        let covered_history_ids = input.covered_history_ids();
        if covered_history_ids.is_empty() {
            return Err(CompactionError::NoCompressibleWindow.into());
        }

        self.validate_window_plan_is_current(input.window_plan())?;
        let turns = self.model_turn_histories(HiddenToolExchangeVisibility::Include, true)?;
        let covered_count = input.window_plan().covered_turn_ids().len();
        let current_history_ids = turns
            .iter()
            .take(covered_count)
            .flat_map(|turn| turn.items.iter().map(|record| record.item.history_id))
            .collect::<BTreeSet<_>>();
        if &current_history_ids != covered_history_ids {
            return Err(CompactionError::StaleWindow.into());
        }
        input
            .window_plan()
            .new_boundary()
            .ok_or_else(|| RuntimeError::from(CompactionError::NoCompressibleWindow))
    }

    fn validate_window_plan_is_current(
        &self,
        plan: &CompactionWindowPlan,
    ) -> Result<(), RuntimeError> {
        if self.compaction_window_fingerprint()? != plan.fingerprint() {
            return Err(CompactionError::StaleWindow.into());
        }
        let turns = self.model_turn_histories(HiddenToolExchangeVisibility::Include, true)?;
        let covered_count = plan.covered_turn_ids().len();
        let current_covered_ids = turns
            .iter()
            .take(covered_count)
            .map(|turn| turn.id)
            .collect::<Vec<_>>();
        let current_retained_ids = turns
            .iter()
            .skip(covered_count)
            .map(|turn| turn.id)
            .collect::<Vec<_>>();
        if current_covered_ids != plan.covered_turn_ids()
            || current_retained_ids != plan.retained_turn_ids()
            || current_covered_ids.last().copied() != plan.new_boundary()
        {
            return Err(CompactionError::StaleWindow.into());
        }
        Ok(())
    }

    pub(crate) fn compaction_window_fingerprint(
        &self,
    ) -> Result<CompactionWindowFingerprint, RuntimeError> {
        let bytes = serde_json::to_vec(&(
            self.transcript.persisted(),
            self.prompt_history_projection,
            self.compacted_checkpoint
                .as_ref()
                .map(CompactedCheckpoint::persisted),
            self.task_anchor.as_ref().map(|anchor| anchor.objective()),
            self.archived_ref_manifest.fingerprint_material(),
        ))
        .map_err(|error| CompactionError::PayloadSerialization {
            message: error.to_string(),
        })?;
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        Ok(CompactionWindowFingerprint::new(hash))
    }

    pub(crate) fn validate_archived_ref_manifest(&self) -> Result<(), RuntimeError> {
        self.validate_archived_ref_manifest_for(
            &self.transcript,
            self.prompt_history_projection,
            &self.archived_ref_manifest,
        )
    }

    pub(crate) fn validate_archived_ref_manifest_for(
        &self,
        transcript: &Transcript,
        prompt_history_projection: PromptHistoryProjection,
        archived_ref_manifest: &ArchivedRefManifest,
    ) -> Result<(), RuntimeError> {
        let expected = self.current_archived_refs_for(transcript, prompt_history_projection)?;
        if expected != archived_ref_manifest.refs() {
            return Err(CompactionError::StaleWindow.into());
        }
        for reference in archived_ref_manifest.refs() {
            self.artifacts
                .validate_text_evidence(reference.evidence())?;
        }
        Ok(())
    }

    fn current_archived_refs_for(
        &self,
        transcript: &Transcript,
        prompt_history_projection: PromptHistoryProjection,
    ) -> Result<Vec<CheckpointRef>, RuntimeError> {
        let compacted_through = prompt_history_projection.compacted_through();
        let mut refs = Vec::new();
        for item in transcript.items() {
            let TranscriptItem::ToolResult {
                id,
                model_turn_id,
                artifact_id,
                prompt_projection: ToolResultPromptProjection::ArtifactNotice,
                ..
            } = item
            else {
                continue;
            };
            if compacted_through.is_some_and(|boundary| *model_turn_id <= boundary) {
                continue;
            }
            refs.push(history_checkpoint_ref(
                *id,
                CheckpointSourceKind::ToolResult,
                artifact_id,
            )?);
        }
        Ok(refs)
    }

    fn provider_history_item_count_for(
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

fn retained_start_for_completed_count(
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

fn existing_archived_tool_call_ids(turns: &[ModelTurnHistory]) -> BTreeSet<ToolCallId> {
    turns
        .iter()
        .flat_map(ModelTurnHistory::existing_archived_tool_call_ids)
        .collect()
}

fn archived_refs_for_plan(
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

fn projected_turn_tokens(
    turns: &[ModelTurnHistory],
    archived_tool_call_ids: &BTreeSet<ToolCallId>,
) -> Result<u64, RuntimeError> {
    turns.iter().try_fold(0_u64, |total, turn| {
        total
            .checked_add(turn.projected_token_estimate(archived_tool_call_ids)?)
            .ok_or_else(|| RuntimeError::from(CompactionError::BudgetOverflow))
    })
}

fn retained_projection_fits(
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

fn compaction_window_plan(
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
