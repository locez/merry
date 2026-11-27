//! Compaction history and exact source-reference projection.

use crate::{
    RuntimeError,
    checkpoint::{
        CheckpointError, CheckpointRef, CheckpointRefId, CheckpointSequenceRange,
        CheckpointSourceKind,
    },
    compaction::{
        CitationCompactionInput, CitationCompactionInputParts, CitationCompactionInputPolicy,
        CitationCompactionModelTurn, CitationCompactionPolicy, CitationCompactionWindowBundle,
        CompactionError, CompactionWindowPlan, ResolvedCitationCompactionBudget,
    },
    session::{
        ModelTurn, ModelTurnId, ModelTurnStatus, PromptHistoryProjection, SessionState, Transcript,
        history::CompactionHistoryItem,
        transcript::{
            ToolCallPromptProjection, ToolResultPromptProjection, TranscriptItem, TranscriptItemId,
        },
    },
    token_estimate::{BYTES_PER_TOKEN, ReasoningTokenAccounting, estimate_text_tokens},
};
use merry_core::ToolCallId;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HiddenToolExchangeVisibility {
    Include,
    Exclude,
}

pub(super) struct ModelTurnHistory<'a> {
    pub(super) id: ModelTurnId,
    pub(super) status: ModelTurnStatus,
    pub(super) items: Vec<CompactionHistoryRecord<'a>>,
    pub(super) projected_reasoning_tokens: u64,
}

/// JSON keys, the turn id, the status tag, and separators one payload turn adds.
const COMPACTION_PAYLOAD_TURN_ENVELOPE_BYTES: u64 = 64;

/// Estimated tokens the covered turns contribute to the compaction payload.
pub(super) fn covered_payload_tokens(
    turns: &[ModelTurnHistory<'_>],
    text_only: bool,
) -> Result<u64, RuntimeError> {
    turns.iter().try_fold(0_u64, |total, turn| {
        total
            .checked_add(turn.compaction_payload_token_estimate(text_only)?)
            .ok_or_else(|| RuntimeError::from(CompactionError::BudgetOverflow))
    })
}

#[derive(Clone)]
pub(super) struct CompactionHistoryRecord<'a> {
    pub(super) item: CompactionHistoryItem<'a>,
    pub(super) reference: CheckpointRef,
}

impl ModelTurnHistory<'_> {
    pub(super) fn compaction_payload_token_estimate(
        &self,
        text_only: bool,
    ) -> Result<u64, RuntimeError> {
        let item_tokens = self
            .items
            .iter()
            .filter(|record| !text_only || !record.item.is_tool_exchange())
            .try_fold(0_u64, |total, record| {
                total
                    .checked_add(record.item.compaction_payload_token_estimate()?)
                    .ok_or_else(|| RuntimeError::from(CompactionError::BudgetOverflow))
            })?;
        Ok(item_tokens
            .saturating_add(COMPACTION_PAYLOAD_TURN_ENVELOPE_BYTES.div_ceil(BYTES_PER_TOKEN)))
    }

    pub(super) fn projected_token_estimate(
        &self,
        archived_tool_call_ids: &BTreeSet<ToolCallId>,
        reasoning_tokens: ReasoningTokenAccounting,
    ) -> Result<u64, RuntimeError> {
        let base = self.items.iter().try_fold(0_u64, |total, record| {
            total
                .checked_add(
                    record
                        .item
                        .projected_token_estimate(archived_tool_call_ids)?,
                )
                .ok_or_else(|| RuntimeError::from(CompactionError::BudgetOverflow))
        })?;
        match reasoning_tokens {
            ReasoningTokenAccounting::Include => base
                .checked_add(self.projected_reasoning_tokens)
                .ok_or_else(|| RuntimeError::from(CompactionError::BudgetOverflow)),
            ReasoningTokenAccounting::Exclude => Ok(base),
        }
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
    ) -> Result<Vec<ModelTurnHistory<'_>>, RuntimeError> {
        self.model_turn_histories_for(
            &self.transcript,
            self.prompt_history_projection,
            hidden_tool_exchanges,
            apply_prompt_projection,
        )
    }

    pub(super) fn model_turn_histories_for<'a>(
        &'a self,
        transcript: &'a Transcript,
        prompt_history_projection: PromptHistoryProjection,
        hidden_tool_exchanges: HiddenToolExchangeVisibility,
        apply_prompt_projection: bool,
    ) -> Result<Vec<ModelTurnHistory<'a>>, RuntimeError> {
        let compacted_through = apply_prompt_projection
            .then(|| prompt_history_projection.compacted_through())
            .flatten();
        transcript
            .model_turns()
            .map_err(|_| RuntimeError::from(CompactionError::StaleWindow))?
            .into_iter()
            .filter(|turn| compacted_through.is_none_or(|boundary| turn.id() > boundary))
            .map(|turn| {
                let projected_reasoning_tokens = projected_reasoning_tokens_for_turn(&turn)?;
                Ok(ModelTurnHistory {
                    id: turn.id(),
                    status: turn.status(),
                    items: self.history_items_for_model_turn(&turn, hidden_tool_exchanges)?,
                    projected_reasoning_tokens,
                })
            })
            .collect()
    }

    pub(super) fn history_items_for_model_turn<'a>(
        &'a self,
        turn: &ModelTurn<'a>,
        hidden_tool_exchanges: HiddenToolExchangeVisibility,
    ) -> Result<Vec<CompactionHistoryRecord<'a>>, RuntimeError> {
        let mut items = Vec::with_capacity(turn.items().len());
        let mut results = BTreeMap::new();
        for &item in turn.items() {
            if let TranscriptItem::ToolResult {
                id,
                call_id,
                result,
                payload,
                prompt_projection,
                ..
            } = item
                && results
                    .insert(call_id.clone(), (*id, result, payload, *prompt_projection))
                    .is_some()
            {
                return Err(CompactionError::StaleWindow.into());
            }
        }
        let mut matched_results = BTreeSet::new();

        for &item in turn.items() {
            match item {
                TranscriptItem::UserMessage { id, text, .. } => {
                    items.push(CompactionHistoryRecord {
                        item: CompactionHistoryItem::user(id.as_u64(), text),
                        reference: history_checkpoint_ref(*id, CheckpointSourceKind::UserMessage)?,
                    });
                }
                TranscriptItem::AssistantText { id, text, .. } => {
                    items.push(CompactionHistoryRecord {
                        item: CompactionHistoryItem::assistant(id.as_u64(), text),
                        reference: history_checkpoint_ref(
                            *id,
                            CheckpointSourceKind::AssistantMessage,
                        )?,
                    });
                }
                TranscriptItem::AssistantReasoning { .. } => {}
                TranscriptItem::ToolCall {
                    call,
                    prompt_projection: call_projection,
                    ..
                } => {
                    let Some(&(id, result, payload, result_projection)) = results.get(call.id())
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
                        | (ToolCallPromptProjection::Full, ToolResultPromptProjection::Collapsed) =>
                            {}
                        (ToolCallPromptProjection::Hidden, _)
                        | (ToolCallPromptProjection::Full, ToolResultPromptProjection::Hidden) => {
                            return Err(CompactionError::StaleWindow.into());
                        }
                    }
                    items.push(CompactionHistoryRecord {
                        item: CompactionHistoryItem::tool_exchange(
                            id.as_u64(),
                            call,
                            result,
                            payload.full(),
                            *call_projection,
                            result_projection,
                            payload.model(),
                        ),
                        reference: history_checkpoint_ref(id, CheckpointSourceKind::ToolResult)?,
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
        covered: &[&ModelTurnHistory<'_>],
        plan: CompactionWindowPlan,
        archived_refs: Vec<CheckpointRef>,
        retained_tool_exchanges: Option<usize>,
    ) -> Result<CitationCompactionInput, RuntimeError> {
        if covered.iter().all(|turn| turn.items.is_empty()) {
            return Err(CompactionError::NoCompressibleWindow.into());
        }

        // A one-shot payload keeps the newest tool exchanges and omits the rest.
        // Counting is per exchange, never per item, because a tool call and its result
        // are one history item and the runtime rejects a window that carries only one
        // of them. Omitting whole exchanges is what makes the payload fit: measured on
        // a real session, covered call arguments alone were 225,800 tokens against a
        // 190,400 token input budget, and keeping a minimal marker per exchange still
        // cost about 95,000 tokens across 1,324 exchanges.
        let retained_exchanges = retained_tool_exchanges.map(|retained| {
            let mut full = BTreeSet::new();
            'covered: for turn in covered.iter().rev() {
                for record in turn.items.iter().rev() {
                    if !record.item.is_tool_exchange() {
                        continue;
                    }
                    if full.len() == retained {
                        break 'covered;
                    }
                    full.insert(record.item.history_id);
                }
            }
            full
        });

        let mut covered_history_ids = BTreeSet::new();
        let checkpoint_id = crate::CheckpointId::new(&format!(
            "checkpoint-{}-{}",
            sanitize_checkpoint_component(self.session_id.as_str()),
            self.transcript.next_id().as_u64()
        ))?;
        let mut refs_by_id = BTreeMap::new();
        let mut window = Vec::with_capacity(covered.len());

        for turn in covered {
            let mut items = Vec::with_capacity(turn.items.len());
            for record in &turn.items {
                // A dropped exchange is still covered history: the plan marks it
                // compacted, it just does not travel through the payload.
                covered_history_ids.insert(record.item.history_id);
                // Only tool exchanges are ever omitted. The conversation text stays,
                // because that is what the summary describes. An exchange is dropped
                // whole: a tool call and its result are one history item, and a window
                // that carried only one of them would be rejected as stale.
                if record.item.is_tool_exchange()
                    && retained_exchanges
                        .as_ref()
                        .is_some_and(|retained| !retained.contains(&record.item.history_id))
                {
                    continue;
                }
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

        Ok(CitationCompactionInput::new(
            CitationCompactionInputParts {
                input_policy: CitationCompactionInputPolicy::new(policy, resolved_budget),
                task_anchor_snapshot: self.task_anchor.clone(),
                manifest,
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

fn projected_reasoning_tokens_for_turn(turn: &ModelTurn<'_>) -> Result<u64, RuntimeError> {
    let items = turn.items();
    let mut total = 0_u64;
    for (index, item) in items.iter().enumerate() {
        let TranscriptItem::AssistantReasoning { text, .. } = item else {
            continue;
        };
        if items[index + 1..]
            .iter()
            .any(|other| other.is_visible_assistant_output())
        {
            total = total
                .checked_add(estimate_text_tokens(text))
                .ok_or_else(|| RuntimeError::from(CompactionError::BudgetOverflow))?;
        }
    }
    Ok(total)
}

pub(super) fn history_checkpoint_ref(
    item_id: TranscriptItemId,
    source_kind: CheckpointSourceKind,
) -> Result<CheckpointRef, CheckpointError> {
    Ok(CheckpointRef::new(
        history_ref_id(item_id)?,
        source_kind,
        CheckpointSequenceRange::new(item_id.as_u64(), item_id.as_u64())?,
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
