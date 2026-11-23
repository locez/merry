use crate::{
    RuntimeError,
    checkpoint::{CheckpointError, CheckpointRef, CheckpointSourceKind},
    compaction::{
        ArchiveOnlyCompactionInput, CitationCompactionInput, CitationCompactionPolicy,
        CompactionCoverageBudget, CompactionError, CompactionOutcome, CompactionPreparation,
        CompactionShape, CompactionWindowBudget, CompactionWindowFingerprint, CompactionWindowPlan,
        ResolvedCitationCompactionBudget,
    },
    context::{CompactedCheckpoint, CompactedCheckpointSummary},
    permission::PermissionReviewContextEntry,
    session::{
        ModelTurnId, PromptHistoryProjection, SessionState, Transcript,
        checkpoint_window::{
            history::{HiddenToolExchangeVisibility, history_checkpoint_ref},
            planning::archived_refs_for_plan,
        },
        history::{permission_review_context_entry, permission_review_task_anchor_entry},
        transcript::{ToolResultPromptProjection, TranscriptItem},
    },
};
use std::collections::BTreeSet;

mod history;

mod planning;

const PERMISSION_REVIEW_RECENT_CONTEXT_LIMIT: usize = 12;
const PERMISSION_REVIEW_PINNED_USER_LIMIT: usize = 4;

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

    fn fingerprint_material(&self) -> Vec<(String, CheckpointSourceKind, u64, u64)> {
        self.refs
            .iter()
            .map(|reference| {
                (
                    reference.id().as_str().to_owned(),
                    reference.source_kind(),
                    reference.sequence_range().start(),
                    reference.sequence_range().end(),
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
    pruned_segment: Option<super::PrunedCompactionSegment>,
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

    pub(crate) fn pruned_segment(&self) -> Option<&super::PrunedCompactionSegment> {
        self.state().pruned_segment.as_ref()
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

    #[cfg(test)]
    pub(crate) fn build_citation_compaction_input(
        &self,
        policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError> {
        let window_budget =
            CompactionWindowBudget::unbounded_for_tests(resolved_budget.output_token_limit())?;
        self.build_citation_compaction_input_with_window_budget(
            policy,
            resolved_budget,
            window_budget,
            CompactionCoverageBudget::unbounded(),
        )
    }

    pub(crate) fn build_citation_compaction_input_with_window_budget(
        &self,
        policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
        window_budget: CompactionWindowBudget,
        coverage: CompactionCoverageBudget,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError> {
        match self.build_compaction_preparation_with_window_budget(
            policy,
            resolved_budget,
            window_budget,
            coverage,
        )? {
            Some(CompactionPreparation::ReplaceCheckpoint(input)) => Ok(Some(*input)),
            Some(CompactionPreparation::ArchiveToolResults(_)) | None => Ok(None),
        }
    }

    /// Builds one compaction preparation that must land inside the body budget.
    pub(crate) fn build_compaction_preparation_with_window_budget(
        &self,
        policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
        window_budget: CompactionWindowBudget,
        coverage: CompactionCoverageBudget,
    ) -> Result<Option<CompactionPreparation>, RuntimeError> {
        self.build_compaction_preparation(
            policy,
            resolved_budget,
            window_budget,
            coverage,
            CompactionShape::SinglePass,
        )
    }

    /// Builds one rolling pass that may leave the retained history above the body budget.
    ///
    /// Each rolling pass covers as much history as the compaction window can host,
    /// and the runtime repeats until the recompiled request lands back under the
    /// body budget. This is what lets a session keep compacting after its context
    /// window shrinks, when the retained history alone no longer fits the budget.
    pub(crate) fn build_rolling_compaction_preparation(
        &self,
        policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
        window_budget: CompactionWindowBudget,
        coverage: CompactionCoverageBudget,
    ) -> Result<Option<CompactionPreparation>, RuntimeError> {
        self.build_compaction_preparation(
            policy,
            resolved_budget,
            window_budget,
            coverage,
            CompactionShape::Rolling,
        )
    }

    /// Builds one one-shot pass that covers everything before the retained tail.
    ///
    /// The pass keeps the newest `retained_tool_exchanges` covered tool exchanges at
    /// full length and omits older exchanges entirely from the model payload. The payload therefore stops
    /// growing with the number of tool calls in the covered history. This is what a
    /// window that shrank far below the history needs, because rolling would
    /// re-summarize the previous checkpoint on every pass.
    pub(crate) fn build_one_shot_compaction_preparation(
        &self,
        policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
        window_budget: CompactionWindowBudget,
        coverage: CompactionCoverageBudget,
        retained_tool_exchanges: usize,
    ) -> Result<Option<CompactionPreparation>, RuntimeError> {
        self.build_compaction_preparation(
            policy,
            resolved_budget,
            window_budget,
            coverage,
            CompactionShape::OneShot {
                retained_tool_exchanges,
            },
        )
    }

    pub(crate) fn build_compaction_preparation(
        &self,
        policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
        window_budget: CompactionWindowBudget,
        coverage: CompactionCoverageBudget,
        shape: CompactionShape,
    ) -> Result<Option<CompactionPreparation>, RuntimeError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(CompactionError::PendingToolCalls.into());
        }

        let turns = self.model_turn_histories(HiddenToolExchangeVisibility::Include, true)?;
        let Some(plan) =
            self.plan_compaction_window_from_turns(policy, window_budget, coverage, shape, &turns)?
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
            shape.retained_tool_exchanges(),
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
        self.plan_compaction_window_from_turns(
            policy,
            window_budget,
            CompactionCoverageBudget::unbounded(),
            CompactionShape::SinglePass,
            &turns,
        )
    }

    #[cfg(test)]
    pub(crate) fn install_summary_compaction(
        &mut self,
        input: CitationCompactionInput,
        summary: &str,
    ) -> Result<CompactionOutcome, RuntimeError> {
        let prepared = self.prepare_summary_compaction_install(input, summary)?;
        self.revalidate_prepared_compaction_install(&prepared)?;
        Ok(self
            .commit_prepared_compaction_install(prepared)
            .expect("prepared checkpoint replacement must carry an outcome"))
    }

    #[cfg(test)]
    pub(crate) fn prepare_summary_compaction_install(
        &self,
        input: CitationCompactionInput,
        summary: &str,
    ) -> Result<PreparedCompactionInstall, RuntimeError> {
        self.prepare_compaction_install(input, summary, &crate::HistoryRetention::Full)
    }

    pub(crate) fn prepare_summary_compaction_install_with_retention(
        &self,
        input: CitationCompactionInput,
        summary: &str,
        retention: &crate::HistoryRetention,
    ) -> Result<PreparedCompactionInstall, RuntimeError> {
        self.prepare_compaction_install(input, summary, retention)
    }

    fn prepare_compaction_install(
        &self,
        input: CitationCompactionInput,
        summary: &str,
        retention: &crate::HistoryRetention,
    ) -> Result<PreparedCompactionInstall, RuntimeError> {
        if !self.pending_tool_calls.is_empty() {
            return Err(CompactionError::PendingToolCalls.into());
        }
        let compacted_through = self.validate_compaction_window_is_current(&input)?;
        let original_fingerprint = input.window_plan().fingerprint();
        let next_archive_manifest = ArchivedRefManifest::new(input.archived_refs().to_vec())?;

        let checkpoint_id = input.manifest().checkpoint_id().clone();
        let compacted = CompactedCheckpoint::new(summary)?;

        let covered_count = input.covered_history_ids().len();
        let mut transcript = self.transcript.clone();
        transcript.archive_tool_results(input.window_plan().archived_tool_call_ids())?;
        let prompt_history_projection = self
            .prompt_history_projection
            .advanced_through(&transcript, compacted_through)?;
        let compacted_checkpoint = Some(compacted);
        let pruned_segment = if retention.prunes_compacted_history() {
            let pruned_items = transcript.prune_through(compacted_through);
            Some(super::PrunedCompactionSegment {
                pruned_items,
                compacted_through,
                summary: summary.to_owned(),
            })
        } else {
            None
        };
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
                pruned_segment,
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
                pruned_segment: None,
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
        let pruned = state.pruned_segment.is_some();
        self.transcript = state.transcript;
        self.prompt_history_projection = state.prompt_history_projection;
        self.compacted_checkpoint = state.compacted_checkpoint;
        self.archived_ref_manifest = state.archived_ref_manifest;
        if pruned {
            self.resolved_tool_calls = self
                .transcript
                .items()
                .iter()
                .filter_map(|item| match item {
                    TranscriptItem::ToolResult { call_id, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .collect();
        }
        outcome
    }

    /// Builds the bounded runtime context snapshot supplied to permission review.
    ///
    /// Always retains the pinned `task_anchor` (when set) and recent user
    /// messages even when a long burst of tool exchanges or compaction would
    /// otherwise push user intent out of the trailing window.
    pub(crate) fn permission_review_context_snapshot(
        &self,
    ) -> Result<Vec<PermissionReviewContextEntry>, RuntimeError> {
        let mut entries = Vec::new();
        if let Some(task_anchor) = self.task_anchor.as_ref() {
            entries.push(permission_review_task_anchor_entry(task_anchor.objective()));
        }

        let items = self
            .model_turn_histories(HiddenToolExchangeVisibility::Include, true)?
            .into_iter()
            .flat_map(|turn| turn.items)
            .collect::<Vec<_>>();
        let start = items
            .len()
            .saturating_sub(PERMISSION_REVIEW_RECENT_CONTEXT_LIMIT);

        let earlier_users = items[..start]
            .iter()
            .filter(|record| record.item.is_user())
            .collect::<Vec<_>>();
        let pinned_start = earlier_users
            .len()
            .saturating_sub(PERMISSION_REVIEW_PINNED_USER_LIMIT);
        for record in &earlier_users[pinned_start..] {
            entries.push(permission_review_context_entry(&record.item));
        }

        let has_visible_user =
            !earlier_users.is_empty() || items[start..].iter().any(|record| record.item.is_user());
        if !has_visible_user && self.prompt_history_projection.compacted_through().is_some() {
            let full_users = self
                .model_turn_histories(HiddenToolExchangeVisibility::Include, false)?
                .into_iter()
                .flat_map(|turn| turn.items)
                .filter(|record| record.item.is_user())
                .collect::<Vec<_>>();
            let fallback_start = full_users
                .len()
                .saturating_sub(PERMISSION_REVIEW_PINNED_USER_LIMIT);
            for record in &full_users[fallback_start..] {
                entries.push(permission_review_context_entry(&record.item));
            }
        }

        entries.extend(
            items[start..]
                .iter()
                .map(|record| permission_review_context_entry(&record.item)),
        );
        Ok(entries)
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
        let mut hash = 0xcbf29ce484222325_u64;
        hash_transcript(&self.transcript, &mut hash)?;
        let side_bytes = serde_json::to_vec(&(
            self.prompt_history_projection,
            self.compacted_checkpoint
                .as_ref()
                .map(CompactedCheckpoint::text),
            self.task_anchor.as_ref().map(|anchor| anchor.objective()),
            self.archived_ref_manifest.fingerprint_material(),
        ))
        .map_err(|error| CompactionError::PayloadSerialization {
            message: error.to_string(),
        })?;
        fnv_update(&mut hash, &side_bytes);
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
        Ok(())
    }

    fn current_archived_refs_for(
        &self,
        transcript: &Transcript,
        prompt_history_projection: PromptHistoryProjection,
    ) -> Result<Vec<CheckpointRef>, RuntimeError> {
        archived_refs_after_projection(transcript, prompt_history_projection)
    }
}

pub(super) fn archived_refs_after_projection(
    transcript: &Transcript,
    prompt_history_projection: PromptHistoryProjection,
) -> Result<Vec<CheckpointRef>, RuntimeError> {
    let compacted_through = prompt_history_projection.compacted_through();
    let mut refs = Vec::new();
    for item in transcript.items() {
        let TranscriptItem::ToolResult {
            id,
            model_turn_id,
            prompt_projection: ToolResultPromptProjection::Collapsed,
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
        )?);
    }
    Ok(refs)
}

fn fnv_update(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

fn fnv_u64(hash: &mut u64, value: u64) {
    fnv_update(hash, &value.to_le_bytes());
}

fn hash_transcript(transcript: &Transcript, hash: &mut u64) -> Result<(), RuntimeError> {
    fnv_u64(hash, transcript.next_id().as_u64());
    fnv_u64(hash, transcript.next_model_turn_id().as_u64());
    fnv_u64(hash, transcript.items().len() as u64);
    for item in transcript.items() {
        fnv_u64(hash, item.id().as_u64());
        fnv_u64(hash, item.model_turn_id().as_u64());
        match item {
            TranscriptItem::UserMessage {
                text,
                images,
                origin,
                ..
            } => {
                fnv_update(hash, &[1]);
                fnv_update(hash, text.as_bytes());
                fnv_u64(hash, images.len() as u64);
                for image in images {
                    fnv_update(hash, image.label().as_bytes());
                    fnv_update(hash, image.png_bytes());
                    fnv_u64(hash, u64::from(image.width()));
                    fnv_u64(hash, u64::from(image.height()));
                }
                fnv_update(
                    hash,
                    &[match origin {
                        super::transcript::UserInputOrigin::ExternalUser => 1,
                        super::transcript::UserInputOrigin::RuntimeControl => 2,
                    }],
                );
            }
            TranscriptItem::AssistantText { text, .. } => {
                fnv_update(hash, &[2]);
                fnv_update(hash, text.as_bytes());
            }
            TranscriptItem::ToolCall {
                call,
                prompt_projection,
                ..
            } => {
                fnv_update(hash, &[3]);
                let call_bytes = serde_json::to_vec(call).map_err(|error| {
                    CompactionError::PayloadSerialization {
                        message: error.to_string(),
                    }
                })?;
                fnv_update(hash, &call_bytes);
                fnv_update(
                    hash,
                    &[match prompt_projection {
                        super::transcript::ToolCallPromptProjection::Full => 1,
                        super::transcript::ToolCallPromptProjection::Hidden => 2,
                    }],
                );
            }
            TranscriptItem::ToolResult {
                call_id,
                result,
                payload,
                prompt_projection,
                ..
            } => {
                fnv_update(hash, &[4]);
                fnv_update(hash, call_id.as_str().as_bytes());
                let result_bytes = serde_json::to_vec(result).map_err(|error| {
                    CompactionError::PayloadSerialization {
                        message: error.to_string(),
                    }
                })?;
                fnv_update(hash, &result_bytes);
                fnv_update(hash, payload.full().as_str().as_bytes());
                if let Some(model) = payload.model() {
                    fnv_update(hash, &[1]);
                    fnv_update(hash, model.as_str().as_bytes());
                } else {
                    fnv_update(hash, &[0]);
                }
                fnv_u64(hash, payload.images().len() as u64);
                for image in payload.images() {
                    fnv_update(hash, image.label().as_bytes());
                    fnv_update(hash, image.png_bytes());
                }
                fnv_update(
                    hash,
                    &[match prompt_projection {
                        ToolResultPromptProjection::Full => 1,
                        ToolResultPromptProjection::Collapsed => 2,
                        ToolResultPromptProjection::Hidden => 3,
                    }],
                );
            }
        }
    }
    for (turn_id, status) in transcript.model_turn_statuses() {
        fnv_u64(hash, turn_id.as_u64());
        fnv_update(
            hash,
            &[match status {
                super::ModelTurnStatus::InProgress => 1,
                super::ModelTurnStatus::AwaitingToolResults => 2,
                super::ModelTurnStatus::Completed => 3,
                super::ModelTurnStatus::Aborted => 4,
            }],
        );
    }
    for (turn_id, seq) in transcript.model_turn_sequences() {
        fnv_u64(hash, turn_id.as_u64());
        fnv_u64(hash, *seq);
    }
    Ok(())
}
