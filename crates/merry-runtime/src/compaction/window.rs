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
            max_dynamic_body_tokens,
            replacement_fixed_dynamic_body_tokens,
            archive_only_fixed_dynamic_body_tokens,
            checkpoint_output_ceiling_tokens,
        })
    }

    pub(crate) fn unbounded_for_manual_compaction(
        checkpoint_output_ceiling_tokens: u64,
    ) -> Result<Self, CompactionError> {
        Self::new(u64::MAX, u64::MAX, 0, 0, checkpoint_output_ceiling_tokens)
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

pub(crate) fn retained_turn_fallbacks(configured: usize, available_completed: usize) -> Vec<usize> {
    let first = configured.min(available_completed);
    if first == 0 {
        return Vec::new();
    }
    let mut counts = Vec::with_capacity(4);
    for count in [first, 5, 3, 1] {
        if count <= first && !counts.contains(&count) {
            counts.push(count);
        }
    }
    counts
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
