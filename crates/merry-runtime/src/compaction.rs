//! Citation-backed checkpoint compaction input construction.

use crate::{
    checkpoint::{CheckpointId, CheckpointRef, CheckpointRefManifest},
    context::TaskAnchor,
    token_estimate::estimate_text_tokens,
};
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
        "compaction ran {passes} times and the request is still about {estimated_tokens} tokens, above the {body_budget_tokens} token body budget. Raise the context window, or shorten the fixed prompt and the current request"
    )]
    ConvergenceExhausted {
        passes: usize,
        estimated_tokens: u64,
        body_budget_tokens: u64,
    },

    #[error("compaction payload serialization failed: {message}")]
    PayloadSerialization { message: String },

    #[error("compaction window is stale")]
    StaleWindow,

    #[error(
        "this turn cannot fit in the context window: it is about {estimated_tokens} tokens and the limit is {limit_tokens}. Compaction cannot shrink the fixed prompt or this turn. Raise the context window, or send a shorter request"
    )]
    UncompressibleCurrentInput {
        estimated_tokens: u64,
        limit_tokens: u64,
    },

    #[error(
        "the newest completed turn cannot fit under the {limit_tokens} token context limit. Compaction cannot keep a smaller raw turn. Raise the context window"
    )]
    MinimumRawTurnCannotFit { limit_tokens: u64 },

    #[error("compaction model response shape is invalid: {reason}")]
    InvalidModelResponseShape { reason: &'static str },
}

#[path = "compaction/segments.rs"]
mod segments;
#[path = "compaction/summary.rs"]
mod summary;
#[path = "compaction/window.rs"]
mod window;

#[path = "compaction/runner.rs"]
mod runner;

pub(crate) use runner::{
    compaction_model_window, compaction_request_required_tokens,
    generate_validated_compaction_candidate, validate_compaction_model_window,
};
pub use summary::citation_compaction_tail_directive;

mod budget;
mod policy;
mod request;
pub(crate) use budget::{
    CompactionReasoningReserve, compaction_window_safety_tokens, tightened_covered_budget,
};
pub(crate) use policy::CitationCompactionInputPolicy;
pub use policy::{CitationCompactionPolicy, ResolvedCitationCompactionBudget};
pub(crate) use request::{
    CompactionRequestMode, CompactionRequestProjection, CompactionRequestSource,
    compile_citation_compaction_model_request,
};
pub(crate) use segments::persist_covered_segment;
pub(crate) use summary::{format_compact_summary, format_continuation, is_degenerate_summary};

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
    window: Vec<CitationCompactionModelTurn>,
    manifest: CheckpointRefManifest,
    covered_history_ids: BTreeSet<u64>,
    task_anchor_snapshot: Option<TaskAnchor>,
    window_plan: CompactionWindowPlan,
    archived_refs: Vec<CheckpointRef>,
    resolved_budget: ResolvedCitationCompactionBudget,
}

pub(crate) struct CitationCompactionInputParts {
    pub(crate) input_policy: CitationCompactionInputPolicy,
    pub(crate) task_anchor_snapshot: Option<TaskAnchor>,
    pub(crate) manifest: CheckpointRefManifest,
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
        } = parts;
        let CitationCompactionWindowBundle {
            covered_history_ids,
            window,
            window_plan,
            archived_refs,
        } = window_bundle;
        let CitationCompactionInputPolicy { resolved_budget } = input_policy;

        Self {
            window,
            manifest,
            covered_history_ids,
            task_anchor_snapshot,
            window_plan,
            archived_refs,
            resolved_budget,
        }
    }

    /// Bounds retention fitting by exchanges present, not an arbitrarily large configuration.
    pub(crate) fn payload_tool_exchange_count(&self) -> usize {
        self.window
            .iter()
            .map(CitationCompactionModelTurn::tool_exchange_count)
            .sum()
    }

    /// Estimated tokens of the covered history the fallback request would send.
    ///
    /// The runtime uses this to decide how much covered history to give up when a
    /// compaction request does not fit the compaction model window. It measures
    /// the same markdown the payload-mode request shows the model.
    pub(crate) fn covered_payload_token_estimate(&self) -> Result<u64, CompactionError> {
        Ok(estimate_text_tokens(&self.render_covered_segment()))
    }

    #[must_use]
    pub fn manifest(&self) -> &CheckpointRefManifest {
        &self.manifest
    }

    /// Covered turns the summary request describes.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn covered_window(&self) -> &[CitationCompactionModelTurn] {
        &self.window
    }

    /// Markdown of the covered turns, written beside the session when a store exists.
    #[must_use]
    pub fn render_covered_segment(&self) -> String {
        self.window
            .iter()
            .map(CitationCompactionModelTurn::render_markdown)
            .collect::<Vec<_>>()
            .join("\n\n")
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

    pub(crate) fn window_plan(&self) -> &CompactionWindowPlan {
        &self.window_plan
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
