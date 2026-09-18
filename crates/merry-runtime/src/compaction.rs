//! Citation-backed checkpoint compaction input construction.

use crate::{
    checkpoint::{
        CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest, CheckpointSourceKind,
        CitationBackedCheckpoint,
    },
    context::TaskAnchor,
    token_estimate::estimate_text_tokens,
};
use merry_core::EvidenceRef;
use schemars::Schema;
use serde::Serialize;
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CompactionError {
    #[error("compaction policy field {field} must be greater than zero")]
    InvalidPolicy { field: &'static str },

    #[error("summary budget {summary_tokens} exceeds compactor output limit {model_limit_tokens}")]
    OutputBudgetExceedsModelLimit {
        /// Rendered summary ceiling the runtime asked for.
        summary_tokens: u64,
        /// Maximum output tokens the compaction model declared.
        model_limit_tokens: u64,
    },

    #[error("compaction budget arithmetic overflowed")]
    BudgetOverflow,

    #[error("cannot compact while pending tool calls exist")]
    PendingToolCalls,

    #[error("no compressible history exists before retained model turns")]
    NoCompressibleWindow,

    #[error("no compaction window fits the compaction request budget")]
    NoWindowFitsCompactionRequest,

    #[error(
        "compaction cannot reduce the request below the hard watermark after {passes} passes: estimated {estimated_tokens} tokens, limit {hard_limit_tokens}"
    )]
    ConvergenceExhausted {
        passes: usize,
        estimated_tokens: u64,
        hard_limit_tokens: u64,
    },

    #[error("compaction payload serialization failed: {message}")]
    PayloadSerialization { message: String },

    #[error("compaction window is stale")]
    StaleWindow,

    #[error("current input and fixed dynamic context cannot fit below the hard watermark")]
    UncompressibleCurrentInput,

    #[error("the minimum retained raw completed turn cannot fit below the hard watermark")]
    MinimumRawTurnCannotFit,

    #[error(
        "rendered checkpoint is estimated at {estimated_tokens} tokens, above hard summary limit {max_tokens}"
    )]
    RenderedCheckpointTooLarge {
        estimated_tokens: u64,
        max_tokens: u64,
    },

    #[error("compaction model response shape is invalid: {reason}")]
    InvalidModelResponseShape { reason: &'static str },

    #[error("compaction response schema construction failed: {reason}")]
    ResponseSchema { reason: &'static str },
}

#[path = "compaction/window.rs"]
mod window;

#[path = "compaction/prompt.rs"]
mod prompt;

#[path = "compaction/schema.rs"]
mod schema;

#[path = "compaction/runner.rs"]
mod runner;

pub use prompt::{
    COMPACTION_PAYLOAD_TAG, citation_compaction_tail_directive, compaction_payload_block,
};
pub(crate) use runner::{
    compaction_model_window, compaction_request_required_tokens,
    generate_validated_compaction_candidate, validate_compaction_model_window,
};
pub use schema::citation_compaction_response_schema;

mod budget;
mod policy;
mod repair;
mod request;
mod validation;
pub(crate) use budget::{
    CompactionReasoningReserve, compaction_window_safety_tokens, tightened_covered_budget,
};
pub(crate) use policy::CitationCompactionInputPolicy;
pub use policy::{CitationCompactionPolicy, ResolvedCitationCompactionBudget};
pub(crate) use repair::compaction_repair_reserve_tokens;
pub(crate) use request::{
    CompactionRequestMode, CompactionRequestProjection, CompactionRequestSource,
    compile_citation_compaction_model_request,
};
pub(crate) use validation::checkpoint_from_candidate_json;

pub(crate) use window::{
    ArchiveOnlyCompactionInput, CitationCompactionModelTurn, CitationCompactionToolResult,
    CitationCompactionTurnItem, CompactionCoverageBudget, CompactionShape, CompactionWindowBudget,
    CompactionWindowFingerprint, CompactionWindowPlan, RetainedFit, retained_turn_fallbacks,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutcome {
    checkpoint_id: CheckpointId,
    covered_model_turn_count: usize,
    covered_history_item_count: usize,
    retained_history_item_count: usize,
}

impl CompactionOutcome {
    /// Aggregates rolling coverage while keeping the final checkpoint and retained tail.
    pub(crate) fn followed_by(self, next: Self) -> Result<Self, CompactionError> {
        Ok(Self {
            covered_model_turn_count: self
                .covered_model_turn_count
                .checked_add(next.covered_model_turn_count)
                .ok_or(CompactionError::BudgetOverflow)?,
            covered_history_item_count: self
                .covered_history_item_count
                .checked_add(next.covered_history_item_count)
                .ok_or(CompactionError::BudgetOverflow)?,
            ..next
        })
    }

    pub(crate) fn new(
        checkpoint_id: CheckpointId,
        covered_model_turn_count: usize,
        covered_history_item_count: usize,
        retained_history_item_count: usize,
    ) -> Self {
        Self {
            checkpoint_id,
            covered_model_turn_count,
            covered_history_item_count,
            retained_history_item_count,
        }
    }

    #[must_use]
    pub fn checkpoint_id(&self) -> &CheckpointId {
        &self.checkpoint_id
    }

    /// Number of model turns newly covered by this compaction.
    #[must_use]
    pub fn covered_model_turn_count(&self) -> usize {
        self.covered_model_turn_count
    }

    #[must_use]
    pub fn covered_history_item_count(&self) -> usize {
        self.covered_history_item_count
    }

    #[must_use]
    pub fn retained_history_item_count(&self) -> usize {
        self.retained_history_item_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationCompactionInput {
    payload: CitationCompactionPayload,
    manifest: CheckpointRefManifest,
    covered_history_ids: BTreeSet<u64>,
    task_anchor_snapshot: Option<TaskAnchor>,
    previous_checkpoint_snapshot: Option<CitationBackedCheckpoint>,
    window_plan: CompactionWindowPlan,
    model_supplied_ref_ids: BTreeSet<CheckpointRefId>,
    pinned_refs: BTreeSet<crate::CheckpointRefId>,
    archived_refs: Vec<CheckpointRef>,
    resolved_budget: ResolvedCitationCompactionBudget,
}

pub(crate) struct CitationCompactionInputParts {
    pub(crate) input_policy: CitationCompactionInputPolicy,
    pub(crate) task_anchor_snapshot: Option<TaskAnchor>,
    pub(crate) manifest: CheckpointRefManifest,
    pub(crate) previous_checkpoint: Option<CitationCompactionPreviousCheckpoint>,
    pub(crate) previous_checkpoint_snapshot: Option<CitationBackedCheckpoint>,
}

pub(crate) struct CitationCompactionWindowBundle {
    pub(crate) covered_history_ids: BTreeSet<u64>,
    pub(crate) window: Vec<CitationCompactionModelTurn>,
    pub(crate) window_plan: CompactionWindowPlan,
    pub(crate) archived_refs: Vec<CheckpointRef>,
}

impl CitationCompactionInput {
    pub(crate) fn new(
        parts: CitationCompactionInputParts,
        window_bundle: CitationCompactionWindowBundle,
    ) -> Self {
        let CitationCompactionInputParts {
            input_policy,
            task_anchor_snapshot,
            manifest,
            previous_checkpoint,
            previous_checkpoint_snapshot,
        } = parts;
        let CitationCompactionWindowBundle {
            covered_history_ids,
            window,
            window_plan,
            archived_refs,
        } = window_bundle;
        let CitationCompactionInputPolicy { resolved_budget } = input_policy;
        let model_supplied_ref_names = previous_checkpoint
            .iter()
            .flat_map(CitationCompactionPreviousCheckpoint::original_ref_ids)
            .chain(window.iter().flat_map(CitationCompactionModelTurn::ref_ids))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let model_supplied_ref_ids: BTreeSet<CheckpointRefId> = manifest
            .refs()
            .iter()
            .filter(|reference| model_supplied_ref_names.contains(reference.id().as_str()))
            .map(|reference| reference.id().clone())
            .collect();
        let available_ref_ids = model_supplied_ref_ids
            .iter()
            .map(|ref_id| ref_id.as_str().to_owned())
            .collect();
        let pinned_refs = archived_refs
            .iter()
            .map(|reference| reference.id().clone())
            .collect();
        let payload = CitationCompactionPayload {
            policy: CitationCompactionPayloadPolicy {
                target_output_tokens: resolved_budget.target_output_tokens(),
                max_output_tokens: resolved_budget.output_token_limit(),
                max_accepted_output_bytes: resolved_budget.max_accepted_output_bytes(),
            },
            control: CitationCompactionControl {
                task_anchor: task_anchor_snapshot
                    .as_ref()
                    .map(|anchor| anchor.objective().to_owned()),
                current_user_input_excluded: true,
            },
            available_ref_ids,
            previous_checkpoint,
            window,
        };

        Self {
            payload,
            manifest,
            covered_history_ids,
            task_anchor_snapshot,
            previous_checkpoint_snapshot,
            window_plan,
            model_supplied_ref_ids,
            pinned_refs,
            archived_refs,
            resolved_budget,
        }
    }

    pub fn to_model_payload_json(&self) -> Result<String, CompactionError> {
        serde_json::to_string(&self.payload).map_err(|error| {
            CompactionError::PayloadSerialization {
                message: error.to_string(),
            }
        })
    }

    /// Bounds retention fitting by exchanges present, not an arbitrarily large configuration.
    pub(crate) fn payload_tool_exchange_count(&self) -> usize {
        self.payload
            .window
            .iter()
            .map(CitationCompactionModelTurn::tool_exchange_count)
            .sum()
    }

    /// Estimated tokens the covered turns contribute to the serialized payload.
    ///
    /// The runtime uses this to decide how much covered history to give up when a
    /// compaction request does not fit the compaction model window. It measures
    /// the serialized payload with and without the covered window, so it stays
    /// consistent with the request the runtime is about to send.
    pub(crate) fn covered_payload_token_estimate(&self) -> Result<u64, CompactionError> {
        let full = estimate_text_tokens(&self.to_model_payload_json()?);
        let mut payload = self.payload.clone();
        payload.window.clear();
        let fixed = serde_json::to_string(&payload).map_err(|error| {
            CompactionError::PayloadSerialization {
                message: error.to_string(),
            }
        })?;
        Ok(full.saturating_sub(estimate_text_tokens(&fixed)))
    }

    /// Builds the exact structured-output schema for the references visible in
    /// this compaction input.
    pub fn model_response_schema(&self) -> Result<Schema, CompactionError> {
        let available_ref_ids = self.available_ref_ids();
        schema::citation_compaction_response_schema_for_refs(&available_ref_ids)
            .map_err(|reason| CompactionError::ResponseSchema { reason })
    }

    #[must_use]
    pub fn manifest(&self) -> &CheckpointRefManifest {
        &self.manifest
    }

    #[must_use]
    pub fn covered_history_ids(&self) -> &BTreeSet<u64> {
        &self.covered_history_ids
    }

    #[must_use]
    pub fn task_anchor_snapshot(&self) -> Option<&TaskAnchor> {
        self.task_anchor_snapshot.as_ref()
    }

    #[must_use]
    pub fn resolved_budget(&self) -> ResolvedCitationCompactionBudget {
        self.resolved_budget
    }

    pub(crate) fn previous_checkpoint_snapshot(&self) -> Option<&CitationBackedCheckpoint> {
        self.previous_checkpoint_snapshot.as_ref()
    }

    pub(crate) fn window_plan(&self) -> &CompactionWindowPlan {
        &self.window_plan
    }

    fn model_supplied_ref_ids(&self) -> &BTreeSet<CheckpointRefId> {
        &self.model_supplied_ref_ids
    }

    fn available_ref_ids(&self) -> Vec<&str> {
        self.payload
            .available_ref_ids
            .iter()
            .map(String::as_str)
            .collect()
    }

    pub(crate) fn pinned_refs(&self) -> &BTreeSet<crate::CheckpointRefId> {
        &self.pinned_refs
    }

    pub(crate) fn archived_refs(&self) -> &[CheckpointRef] {
        &self.archived_refs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompactionPreparation {
    ReplaceCheckpoint(Box<CitationCompactionInput>),
    ArchiveToolResults(ArchiveOnlyCompactionInput),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CitationCompactionPreviousCheckpointInput<'a> {
    CitationBacked(&'a CitationBackedCheckpoint),
    PlainText { text: &'a str },
}

pub(crate) fn previous_checkpoint_payload(
    input: CitationCompactionPreviousCheckpointInput<'_>,
) -> CitationCompactionPreviousCheckpoint {
    match input {
        CitationCompactionPreviousCheckpointInput::CitationBacked(checkpoint) => {
            let original_ref_ids = checkpoint
                .sections()
                .iter()
                .flat_map(|(_, entry)| entry.refs().iter().cloned())
                .collect::<BTreeSet<_>>();
            CitationCompactionPreviousCheckpoint {
                checkpoint_id: checkpoint.id().as_str().to_owned(),
                estimated_tokens: estimate_text_tokens(&checkpoint.render_prompt_text()),
                text: None,
                entries: checkpoint
                    .sections()
                    .iter()
                    .map(|(section, entry)| CitationCompactionPriorEntry {
                        entry_id: entry.id().as_str().to_owned(),
                        estimated_tokens: estimate_text_tokens(&entry.render_prompt_text()),
                        section: section.as_str().to_owned(),
                        text: entry.text().to_owned(),
                        rationale: entry.rationale().map(str::to_owned),
                        refs: entry
                            .refs()
                            .iter()
                            .map(|ref_id| ref_id.as_str())
                            .map(str::to_owned)
                            .collect(),
                    })
                    .collect(),
                original_ref_manifest: Some(CitationCompactionOriginalRefManifest {
                    checkpoint_id: checkpoint.manifest().checkpoint_id().as_str().to_owned(),
                    refs: checkpoint
                        .manifest()
                        .refs()
                        .iter()
                        .filter(|reference| original_ref_ids.contains(reference.id()))
                        .map(CitationCompactionOriginalRef::from)
                        .collect(),
                }),
            }
        }
        CitationCompactionPreviousCheckpointInput::PlainText { text } => {
            CitationCompactionPreviousCheckpoint {
                checkpoint_id: "plain-text-checkpoint".to_owned(),
                estimated_tokens: estimate_text_tokens(text),
                text: Some(text.to_owned()),
                entries: Vec::new(),
                original_ref_manifest: None,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CitationCompactionPayload {
    policy: CitationCompactionPayloadPolicy,
    control: CitationCompactionControl,
    available_ref_ids: Vec<String>,
    previous_checkpoint: Option<CitationCompactionPreviousCheckpoint>,
    window: Vec<CitationCompactionModelTurn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CitationCompactionPayloadPolicy {
    target_output_tokens: u64,
    max_output_tokens: u64,
    max_accepted_output_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CitationCompactionControl {
    task_anchor: Option<String>,
    current_user_input_excluded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CitationCompactionPreviousCheckpoint {
    checkpoint_id: String,
    estimated_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    entries: Vec<CitationCompactionPriorEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_ref_manifest: Option<CitationCompactionOriginalRefManifest>,
}

impl CitationCompactionPreviousCheckpoint {
    fn original_ref_ids(&self) -> impl Iterator<Item = &str> {
        self.original_ref_manifest
            .iter()
            .flat_map(|manifest| manifest.refs.iter().map(|reference| reference.id.as_str()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CitationCompactionOriginalRefManifest {
    checkpoint_id: String,
    refs: Vec<CitationCompactionOriginalRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CitationCompactionOriginalRef {
    id: String,
    source_kind: CheckpointSourceKind,
    sequence_start: u64,
    sequence_end: u64,
    evidence: EvidenceRef,
}

impl From<&CheckpointRef> for CitationCompactionOriginalRef {
    fn from(reference: &CheckpointRef) -> Self {
        Self {
            id: reference.id().as_str().to_owned(),
            source_kind: reference.source_kind(),
            sequence_start: reference.sequence_range().start(),
            sequence_end: reference.sequence_range().end(),
            evidence: reference.evidence().clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CitationCompactionPriorEntry {
    entry_id: String,
    estimated_tokens: u64,
    section: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rationale: Option<String>,
    refs: Vec<String>,
}

pub(crate) fn bounded_excerpt(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }

    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated]", &text[..end])
}

#[cfg(test)]
#[path = "compaction/budget_tests.rs"]
mod budget_tests;

#[cfg(test)]
mod tests;
