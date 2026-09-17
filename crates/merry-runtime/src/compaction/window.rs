use super::CompactionError;
use crate::{
    checkpoint::CheckpointRef,
    session::{ModelTurnId, ModelTurnStatus},
};
use merry_core::{ArtifactId, ToolCallId, ToolCallResultStatus};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompactionWindowBudget {
    primary_window_tokens: u64,
    preferred_dynamic_body_tokens: Option<u64>,
    max_dynamic_body_tokens: u64,
    replacement_fixed_dynamic_body_tokens: u64,
    archive_only_fixed_dynamic_body_tokens: u64,
    checkpoint_output_ceiling_tokens: u64,
}

impl CompactionWindowBudget {
    pub(crate) fn new(
        primary_window_tokens: u64,
        max_dynamic_body_tokens: u64,
        replacement_fixed_dynamic_body_tokens: u64,
        archive_only_fixed_dynamic_body_tokens: u64,
        checkpoint_output_ceiling_tokens: u64,
    ) -> Result<Self, CompactionError> {
        for (field, value) in [
            ("primary_window_tokens", primary_window_tokens),
            ("max_dynamic_body_tokens", max_dynamic_body_tokens),
            (
                "checkpoint_output_ceiling_tokens",
                checkpoint_output_ceiling_tokens,
            ),
        ] {
            if value == 0 {
                return Err(CompactionError::InvalidPolicy { field });
            }
        }

        Ok(Self {
            primary_window_tokens,
            preferred_dynamic_body_tokens: None,
            max_dynamic_body_tokens,
            replacement_fixed_dynamic_body_tokens,
            archive_only_fixed_dynamic_body_tokens,
            checkpoint_output_ceiling_tokens,
        })
    }

    #[cfg(test)]
    pub(crate) fn unbounded_for_tests(
        checkpoint_output_ceiling_tokens: u64,
    ) -> Result<Self, CompactionError> {
        Self::new(u64::MAX, u64::MAX, 0, 0, checkpoint_output_ceiling_tokens)
    }

    /// Adds a bounded raw-history target to fixed input and the summary ceiling.
    /// The hard body budget remains authoritative; arithmetic overflow is rejected.
    pub(crate) fn with_retained_history_target(
        self,
        retained_history_tokens: u64,
    ) -> Result<Self, CompactionError> {
        let preferred_tokens = self
            .replacement_fixed_dynamic_body_tokens
            .checked_add(self.checkpoint_output_ceiling_tokens)
            .and_then(|tokens| tokens.checked_add(retained_history_tokens))
            .ok_or(CompactionError::BudgetOverflow)?;
        Ok(Self {
            preferred_dynamic_body_tokens: Some(preferred_tokens.min(self.max_dynamic_body_tokens)),
            ..self
        })
    }

    /// Returns a stricter copy that uses the preferred body budget, when one exists.
    pub(crate) fn preferred(self) -> Option<Self> {
        self.preferred_dynamic_body_tokens.map(|tokens| Self {
            max_dynamic_body_tokens: tokens,
            preferred_dynamic_body_tokens: None,
            ..self
        })
    }

    pub(crate) const fn primary_window_tokens(self) -> u64 {
        self.primary_window_tokens
    }

    pub(crate) const fn max_dynamic_body_tokens(self) -> u64 {
        self.max_dynamic_body_tokens
    }

    pub(crate) const fn replacement_fixed_dynamic_body_tokens(self) -> u64 {
        self.replacement_fixed_dynamic_body_tokens
    }

    pub(crate) const fn archive_only_fixed_dynamic_body_tokens(self) -> u64 {
        self.archive_only_fixed_dynamic_body_tokens
    }

    pub(crate) const fn checkpoint_output_ceiling_tokens(self) -> u64 {
        self.checkpoint_output_ceiling_tokens
    }
}

/// Upper bound on how much covered history one compaction request may read.
///
/// This bounds the compaction *request*, while [`CompactionWindowBudget`] bounds
/// the request the compaction installs. They answer different questions, so the
/// runtime tracks them separately: a replacement that cannot fit the compaction
/// model window lowers this budget, which keeps more turns raw until the request
/// fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct CompactionCoverageBudget {
    max_tokens: Option<u64>,
}

impl CompactionCoverageBudget {
    /// Keeps the planner's configured retention without a coverage cap.
    pub(crate) const fn unbounded() -> Self {
        Self { max_tokens: None }
    }

    /// Caps the covered payload at `max_tokens`.
    pub(crate) const fn limited(max_tokens: u64) -> Self {
        Self {
            max_tokens: Some(max_tokens),
        }
    }

    /// Returns the cap, or `None` when coverage is unbounded.
    pub(crate) const fn max_tokens(self) -> Option<u64> {
        self.max_tokens
    }
}

/// How one compaction pass chooses what to cover and what the payload carries.
///
/// The runtime selects this from the request it is about to build, so one value
/// describes the whole reduction instead of several loose flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionShape {
    /// One pass that must land inside the body budget; manual compaction.
    SinglePass,
    /// Cover the largest window one request hosts, repeating until the request fits.
    Rolling,
    /// Last resort when even user/assistant history cannot fit in one request.
    RollingText,
    /// Cover everything before the retained tail once, omitting older tool exchanges.
    OneShot {
        /// Newest covered tool exchanges kept, arguments and result together.
        retained_tool_exchanges: usize,
    },
}

impl CompactionShape {
    /// Returns how strictly this shape requires the retained history to fit.
    pub(crate) const fn retained_fit(self) -> RetainedFit {
        match self {
            Self::SinglePass | Self::OneShot { .. } => RetainedFit::Required,
            Self::Rolling | Self::RollingText => RetainedFit::Deferred,
        }
    }

    /// Returns how many covered tool exchanges stay at full length.
    ///
    /// `None` keeps every covered tool exchange at full length.
    pub(crate) const fn retained_tool_exchanges(self) -> Option<usize> {
        match self {
            Self::OneShot {
                retained_tool_exchanges,
            } => Some(retained_tool_exchanges),
            Self::RollingText => Some(0),
            Self::SinglePass | Self::Rolling => None,
        }
    }
}

/// How strictly one compaction pass must leave the retained history inside the body budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetainedFit {
    /// The pass must land under the budget, or report that no window fits.
    ///
    /// Manual compaction uses this: a caller asked for one reduction and needs to
    /// know whether one happened.
    Required,
    /// The pass may land above the budget because the runtime runs another pass.
    ///
    /// Rolling compaction uses this. Each pass covers as much history as the
    /// compaction window can host, and the caller repeats while the recompiled
    /// request still crosses the watermark. Without it, a window that shrank below
    /// the retained history could never reduce anything: every candidate would fail
    /// the budget check before anything could be installed, which is why shrinking
    /// the context window reported that no compaction window fit.
    Deferred,
}

pub(crate) fn retained_turn_fallbacks(configured: usize, available_completed: usize) -> Vec<usize> {
    (1..=configured.min(available_completed)).rev().collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompactionWindowFingerprint(u64);

impl CompactionWindowFingerprint {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionWindowPlan {
    covered_turn_ids: Vec<ModelTurnId>,
    retained_turn_ids: Vec<ModelTurnId>,
    archived_tool_call_ids: BTreeSet<ToolCallId>,
    new_boundary: Option<ModelTurnId>,
    fingerprint: CompactionWindowFingerprint,
}

impl CompactionWindowPlan {
    pub(crate) fn new(
        covered_turn_ids: Vec<ModelTurnId>,
        retained_turn_ids: Vec<ModelTurnId>,
        archived_tool_call_ids: BTreeSet<ToolCallId>,
        new_boundary: Option<ModelTurnId>,
        fingerprint: CompactionWindowFingerprint,
    ) -> Self {
        Self {
            covered_turn_ids,
            retained_turn_ids,
            archived_tool_call_ids,
            new_boundary,
            fingerprint,
        }
    }

    pub(crate) fn covered_turn_ids(&self) -> &[ModelTurnId] {
        &self.covered_turn_ids
    }

    pub(crate) fn retained_turn_ids(&self) -> &[ModelTurnId] {
        &self.retained_turn_ids
    }

    pub(crate) fn archived_tool_call_ids(&self) -> &BTreeSet<ToolCallId> {
        &self.archived_tool_call_ids
    }

    pub(crate) const fn new_boundary(&self) -> Option<ModelTurnId> {
        self.new_boundary
    }

    pub(crate) const fn fingerprint(&self) -> CompactionWindowFingerprint {
        self.fingerprint
    }

    #[cfg(test)]
    pub(crate) fn covered_turn_ids_u64(&self) -> Vec<u64> {
        self.covered_turn_ids
            .iter()
            .map(|turn_id| turn_id.as_u64())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn retained_turn_ids_u64(&self) -> Vec<u64> {
        self.retained_turn_ids
            .iter()
            .map(|turn_id| turn_id.as_u64())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn archived_tool_call_ids_for_tests(&self) -> Vec<ToolCallId> {
        self.archived_tool_call_ids.iter().cloned().collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchiveOnlyCompactionInput {
    window_plan: CompactionWindowPlan,
    archived_refs: Vec<CheckpointRef>,
}

impl ArchiveOnlyCompactionInput {
    pub(crate) fn new(
        window_plan: CompactionWindowPlan,
        archived_refs: Vec<CheckpointRef>,
    ) -> Self {
        Self {
            window_plan,
            archived_refs,
        }
    }

    pub(crate) fn window_plan(&self) -> &CompactionWindowPlan {
        &self.window_plan
    }

    pub(crate) fn archived_refs(&self) -> &[CheckpointRef] {
        &self.archived_refs
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CitationCompactionModelTurn {
    turn_id: u64,
    status: CitationCompactionTurnStatus,
    items: Vec<CitationCompactionTurnItem>,
}

impl CitationCompactionModelTurn {
    pub(crate) fn new(
        turn_id: ModelTurnId,
        status: ModelTurnStatus,
        items: Vec<CitationCompactionTurnItem>,
    ) -> Result<Self, CompactionError> {
        let status = match status {
            ModelTurnStatus::Completed => CitationCompactionTurnStatus::Completed,
            ModelTurnStatus::Aborted => CitationCompactionTurnStatus::Aborted,
            ModelTurnStatus::InProgress | ModelTurnStatus::AwaitingToolResults => {
                return Err(CompactionError::StaleWindow);
            }
        };
        Ok(Self {
            turn_id: turn_id.as_u64(),
            status,
            items,
        })
    }

    pub(crate) fn tool_exchange_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item, CitationCompactionTurnItem::ToolExchange { .. }))
            .count()
    }

    pub(crate) fn ref_ids(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(CitationCompactionTurnItem::ref_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CitationCompactionTurnStatus {
    Completed,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub(crate) enum CitationCompactionTurnItem {
    User {
        history_id: u64,
        ref_id: String,
        text: String,
    },
    Assistant {
        history_id: u64,
        ref_id: String,
        text: String,
    },
    ToolExchange {
        history_id: u64,
        ref_id: String,
        call_id: String,
        name: String,
        arguments: Value,
        result: CitationCompactionToolResult,
    },
}

impl CitationCompactionTurnItem {
    fn ref_id(&self) -> &str {
        match self {
            Self::User { ref_id, .. }
            | Self::Assistant { ref_id, .. }
            | Self::ToolExchange { ref_id, .. } => ref_id,
        }
    }

    pub(crate) fn user(history_id: u64, ref_id: String, text: String) -> Self {
        Self::User {
            history_id,
            ref_id,
            text,
        }
    }

    pub(crate) fn assistant(history_id: u64, ref_id: String, text: String) -> Self {
        Self::Assistant {
            history_id,
            ref_id,
            text,
        }
    }

    pub(crate) fn tool_exchange(
        history_id: u64,
        ref_id: String,
        call_id: &ToolCallId,
        name: String,
        arguments: Value,
        result: CitationCompactionToolResult,
    ) -> Self {
        Self::ToolExchange {
            history_id,
            ref_id,
            call_id: call_id.as_str().to_owned(),
            name,
            arguments,
            result,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CitationCompactionToolResult {
    status: &'static str,
    artifact_id: String,
    content_kind: &'static str,
    content: String,
}

impl CitationCompactionToolResult {
    pub(crate) fn new(
        status: ToolCallResultStatus,
        artifact_id: &ArtifactId,
        content_kind: &'static str,
        content: String,
    ) -> Self {
        Self {
            status: tool_call_result_status_label(status),
            artifact_id: artifact_id.as_str().to_owned(),
            content_kind,
            content,
        }
    }
}

fn tool_call_result_status_label(status: ToolCallResultStatus) -> &'static str {
    match status {
        ToolCallResultStatus::Succeeded => "succeeded",
        ToolCallResultStatus::Failed => "failed",
    }
}
