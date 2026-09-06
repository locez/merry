//! Citation-backed checkpoint compaction input construction.

use crate::{
    RuntimeError,
    checkpoint::{
        CheckpointError, CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest,
        CheckpointSourceKind, CheckpointValidationPolicy, CitationBackedCheckpoint,
        CompactedCheckpointCandidate,
    },
    context::TaskAnchor,
    token_estimate::estimate_text_tokens,
};
use merry_core::EvidenceRef;
use merry_llm::{
    GenerationConfig, ModelContent, ModelError, ModelMessage, ModelMessageRole, ModelName,
    ModelRequest, ModelResponseFormat, ModelStructuredOutputFormat,
};
use schemars::Schema;
use serde::Serialize;
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CompactionError {
    #[error("compaction policy field {field} must be greater than zero")]
    InvalidPolicy { field: &'static str },

    #[error("compaction budget arithmetic overflowed")]
    BudgetOverflow,

    #[error("cannot compact while pending tool calls exist")]
    PendingToolCalls,

    #[error("no compressible history exists before retained model turns")]
    NoCompressibleWindow,

    #[error("compaction payload serialization failed: {message}")]
    PayloadSerialization { message: String },

    #[error("compaction window is stale")]
    StaleWindow,

    #[error("current input and fixed dynamic context cannot fit below the hard watermark")]
    UncompressibleCurrentInput,

    #[error("the minimum retained raw completed turn cannot fit below the hard watermark")]
    MinimumRawTurnCannotFit,

    #[error(
        "rendered checkpoint is estimated at {estimated_tokens} tokens, above output limit {max_tokens}"
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

pub use prompt::citation_compaction_system_prompt;
pub(crate) use runner::{
    generate_validated_compaction_candidate, validate_compaction_model_window,
};
pub use schema::citation_compaction_response_schema;

pub(crate) use window::{
    ArchiveOnlyCompactionInput, CitationCompactionModelTurn, CitationCompactionToolResult,
    CitationCompactionTurnItem, CompactionWindowBudget, CompactionWindowFingerprint,
    CompactionWindowPlan, retained_turn_fallbacks,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CitationCompactionPolicy {
    target_output_tokens: Option<u64>,
    max_accepted_output_bytes: Option<usize>,
    retained_model_turns: usize,
}

const DEFAULT_CHECKPOINT_WINDOW_PERCENT: u64 = 8;
const MIN_CHECKPOINT_OUTPUT_TOKENS: u64 = 2_048;
const MAX_CHECKPOINT_OUTPUT_TOKENS: u64 = 32_768;
const DEFAULT_ACCEPTED_BYTES_PER_TOKEN: u64 = 8;
const DEFAULT_RETAINED_MODEL_TURNS: usize = 5;

impl CitationCompactionPolicy {
    pub fn new(
        target_output_tokens: Option<u64>,
        max_accepted_output_bytes: Option<usize>,
        retained_model_turns: usize,
    ) -> Result<Self, CompactionError> {
        if target_output_tokens == Some(0) {
            return Err(CompactionError::InvalidPolicy {
                field: "target_output_tokens",
            });
        }
        if max_accepted_output_bytes == Some(0) {
            return Err(CompactionError::InvalidPolicy {
                field: "max_accepted_output_bytes",
            });
        }
        if retained_model_turns == 0 {
            return Err(CompactionError::InvalidPolicy {
                field: "retained_model_turns",
            });
        }

        Ok(Self {
            target_output_tokens,
            max_accepted_output_bytes,
            retained_model_turns,
        })
    }

    #[must_use]
    pub fn target_output_tokens(self) -> Option<u64> {
        self.target_output_tokens
    }

    #[must_use]
    pub fn max_accepted_output_bytes(self) -> Option<usize> {
        self.max_accepted_output_bytes
    }

    #[must_use]
    pub fn retained_model_turns(self) -> usize {
        self.retained_model_turns
    }

    pub fn with_retained_model_turns(
        self,
        retained_model_turns: usize,
    ) -> Result<Self, CompactionError> {
        Self::new(
            self.target_output_tokens,
            self.max_accepted_output_bytes,
            retained_model_turns,
        )
    }

    pub fn resolve(
        self,
        primary_window_tokens: u64,
    ) -> Result<ResolvedCitationCompactionBudget, CompactionError> {
        if primary_window_tokens == 0 {
            return Err(CompactionError::InvalidPolicy {
                field: "primary_window_tokens",
            });
        }
        let automatic = primary_window_tokens
            .checked_mul(DEFAULT_CHECKPOINT_WINDOW_PERCENT)
            .and_then(|value| value.checked_div(100))
            .ok_or(CompactionError::BudgetOverflow)?
            .clamp(MIN_CHECKPOINT_OUTPUT_TOKENS, MAX_CHECKPOINT_OUTPUT_TOKENS);
        let output_token_limit = self.target_output_tokens.unwrap_or(automatic);
        let derived_bytes = output_token_limit
            .checked_mul(DEFAULT_ACCEPTED_BYTES_PER_TOKEN)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(CompactionError::BudgetOverflow)?;

        Ok(ResolvedCitationCompactionBudget {
            output_token_limit,
            max_accepted_output_bytes: self.max_accepted_output_bytes.unwrap_or(derived_bytes),
        })
    }
}

impl Default for CitationCompactionPolicy {
    fn default() -> Self {
        Self {
            target_output_tokens: None,
            max_accepted_output_bytes: None,
            retained_model_turns: DEFAULT_RETAINED_MODEL_TURNS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCitationCompactionBudget {
    output_token_limit: u64,
    max_accepted_output_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CitationCompactionInputPolicy {
    resolved_budget: ResolvedCitationCompactionBudget,
}

impl CitationCompactionInputPolicy {
    pub(crate) const fn new(
        _policy: CitationCompactionPolicy,
        resolved_budget: ResolvedCitationCompactionBudget,
    ) -> Self {
        Self { resolved_budget }
    }
}

impl ResolvedCitationCompactionBudget {
    #[must_use]
    pub fn output_token_limit(self) -> u64 {
        self.output_token_limit
    }

    #[must_use]
    pub fn max_accepted_output_bytes(self) -> usize {
        self.max_accepted_output_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutcome {
    checkpoint_id: CheckpointId,
    covered_model_turn_count: usize,
    covered_history_item_count: usize,
    retained_history_item_count: usize,
}

impl CompactionOutcome {
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
                target_output_tokens: resolved_budget.output_token_limit(),
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
                text: None,
                entries: checkpoint
                    .sections()
                    .iter()
                    .map(|(section, entry)| CitationCompactionPriorEntry {
                        entry_id: entry.id().as_str().to_owned(),
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
                text: Some(text.to_owned()),
                entries: Vec::new(),
                original_ref_manifest: None,
            }
        }
    }
}

pub(crate) fn checkpoint_from_candidate_json(
    checkpoint_id: CheckpointId,
    input: &CitationCompactionInput,
    candidate_json: &str,
) -> Result<CitationBackedCheckpoint, RuntimeError> {
    if candidate_json.len() > input.resolved_budget().max_accepted_output_bytes() {
        return Err(CheckpointError::OutputTooLarge {
            actual_bytes: candidate_json.len(),
            max_bytes: input.resolved_budget().max_accepted_output_bytes(),
        }
        .into());
    }

    let mut candidate = CompactedCheckpointCandidate::from_json(candidate_json)?;
    if let Some(previous) = input.previous_checkpoint_snapshot() {
        candidate.materialize_kept_entries(previous);
    }
    validate_candidate_uses_model_supplied_refs(&candidate, input)?;
    let policy = CheckpointValidationPolicy::default();
    let checkpoint = match input.previous_checkpoint_snapshot() {
        Some(previous) => CitationBackedCheckpoint::from_rolling_candidate_with_pinned_refs(
            checkpoint_id,
            candidate,
            input.manifest().clone(),
            previous,
            policy,
            input.pinned_refs(),
        ),
        None => CitationBackedCheckpoint::from_candidate_with_pinned_refs(
            checkpoint_id,
            candidate,
            input.manifest().clone(),
            policy,
            input.pinned_refs(),
        ),
    }
    .map_err(RuntimeError::from)?;
    let estimated_tokens = estimate_text_tokens(&checkpoint.render_prompt_text());
    if estimated_tokens > input.resolved_budget().output_token_limit() {
        return Err(CompactionError::RenderedCheckpointTooLarge {
            estimated_tokens,
            max_tokens: input.resolved_budget().output_token_limit(),
        }
        .into());
    }
    Ok(checkpoint)
}

fn validate_candidate_uses_model_supplied_refs(
    candidate: &CompactedCheckpointCandidate,
    input: &CitationCompactionInput,
) -> Result<(), CheckpointError> {
    for (_, entry) in candidate.sections().iter() {
        for ref_id in entry.refs() {
            if !input.model_supplied_ref_ids().contains(ref_id) {
                return Err(CheckpointError::UnknownRef {
                    entry_id: entry.id().as_str().to_owned(),
                    ref_id: ref_id.as_str().to_owned(),
                });
            }
        }
    }
    Ok(())
}

pub(crate) fn compile_citation_compaction_model_request(
    input: &CitationCompactionInput,
    model: &ModelName,
) -> Result<ModelRequest, ModelError> {
    let payload = input
        .to_model_payload_json()
        .map_err(|error| ModelError::invalid_request(error.to_string()))?;
    let messages = vec![
        ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(citation_compaction_system_prompt())?,
        )?,
        ModelMessage::new(ModelMessageRole::User, ModelContent::text(&payload)?)?,
    ];
    let generation =
        GenerationConfig::new(Some(input.resolved_budget().output_token_limit()), false)?;
    let response_schema = input
        .model_response_schema()
        .map_err(|error| ModelError::invalid_request(error.to_string()))?;
    let response_format = ModelResponseFormat::StructuredOutput(ModelStructuredOutputFormat::new(
        "compacted_checkpoint_candidate",
        response_schema,
    )?);

    ModelRequest::new_with_continuations_and_stable_prefix_and_response_format(
        model.clone(),
        messages,
        Vec::new(),
        Vec::new(),
        generation,
        1,
        Some(response_format),
    )
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
