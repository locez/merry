use crate::{
    RuntimeError,
    artifact::{ArtifactError, TextEvidencePage},
    checkpoint::{CheckpointError, CheckpointRef, CheckpointRefId, CheckpointSourceKind},
    compaction::{
        ArchiveOnlyCompactionInput, CitationCompactionInput, CitationCompactionPolicy,
        CompactionError, CompactionOutcome, CompactionPreparation, CompactionWindowBudget,
        CompactionWindowFingerprint, CompactionWindowPlan, ResolvedCitationCompactionBudget,
        checkpoint_from_candidate_json,
    },
    context::{CompactedCheckpoint, CompactedCheckpointSummary},
    permission::PermissionReviewContextEntry,
    session::{
        ModelTurnId, PromptHistoryProjection, SessionState, Transcript,
        checkpoint_window::{
            history::{HiddenToolExchangeVisibility, history_checkpoint_ref},
            planning::archived_refs_for_plan,
        },
        history::permission_review_context_entry,
        transcript::{ToolResultPromptProjection, TranscriptItem},
    },
};
use merry_core::EvidenceRef;
use std::collections::BTreeSet;

mod history;

mod planning;

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
        let covered_model_turn_count = input.window_plan().covered_turn_ids().len();
        let outcome = CompactionOutcome::new(
            checkpoint_id,
            covered_model_turn_count,
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
}
