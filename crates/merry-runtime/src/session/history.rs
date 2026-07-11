use crate::{
    RuntimeError,
    artifact::ArtifactContent,
    compaction::{CitationCompactionToolResult, CitationCompactionTurnItem, CompactionError},
    permission::PermissionReviewContextEntry,
    token_estimate::estimate_text_tokens,
};
use merry_core::{PendingToolCall, ToolCallResult};
use std::collections::BTreeSet;

use super::transcript::{
    ToolCallPromptProjection, ToolResultPromptProjection, TranscriptItemId,
    archived_tool_result_notice_json,
};

const PERMISSION_REVIEW_ENTRY_MAX_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompactionHistoryItem {
    pub(super) history_id: u64,
    pub(super) kind: CompactionHistoryItemKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CompactionHistoryItemKind {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    ToolExchange {
        call: Box<PendingToolCall>,
        result: Box<ToolCallResult>,
        content: Box<ArtifactContent>,
        call_prompt_projection: ToolCallPromptProjection,
        prompt_projection: ToolResultPromptProjection,
    },
}

impl CompactionHistoryItem {
    pub(super) fn user(history_id: u64, text: String) -> Self {
        Self {
            history_id,
            kind: CompactionHistoryItemKind::User { text },
        }
    }

    pub(super) fn assistant(history_id: u64, text: String) -> Self {
        Self {
            history_id,
            kind: CompactionHistoryItemKind::Assistant { text },
        }
    }

    pub(super) fn tool_exchange(
        history_id: u64,
        call: PendingToolCall,
        result: ToolCallResult,
        content: ArtifactContent,
        call_prompt_projection: ToolCallPromptProjection,
        prompt_projection: ToolResultPromptProjection,
    ) -> Self {
        Self {
            history_id,
            kind: CompactionHistoryItemKind::ToolExchange {
                call: Box::new(call),
                result: Box::new(result),
                content: Box::new(content),
                call_prompt_projection,
                prompt_projection,
            },
        }
    }

    pub(super) fn to_compaction_turn_item(
        &self,
        ref_id: &str,
    ) -> Result<CitationCompactionTurnItem, RuntimeError> {
        let item = match &self.kind {
            CompactionHistoryItemKind::User { text } => {
                CitationCompactionTurnItem::user(self.history_id, ref_id.to_owned(), text.clone())
            }
            CompactionHistoryItemKind::Assistant { text } => CitationCompactionTurnItem::assistant(
                self.history_id,
                ref_id.to_owned(),
                text.clone(),
            ),
            CompactionHistoryItemKind::ToolExchange {
                call,
                result,
                content,
                ..
            } => {
                let (content_kind, content) = exact_artifact_text(content)?;
                CitationCompactionTurnItem::tool_exchange(
                    self.history_id,
                    ref_id.to_owned(),
                    call.id(),
                    call.name().as_str().to_owned(),
                    serde_json::Value::Object(call.arguments().as_object().clone()),
                    CitationCompactionToolResult::new(
                        result.status(),
                        result.artifact().id(),
                        content_kind,
                        content.to_owned(),
                    ),
                )
            }
        };

        Ok(item)
    }

    pub(super) fn projected_token_estimate(
        &self,
        archived_tool_call_ids: &BTreeSet<merry_core::ToolCallId>,
    ) -> Result<u64, RuntimeError> {
        match &self.kind {
            CompactionHistoryItemKind::User { text }
            | CompactionHistoryItemKind::Assistant { text } => Ok(estimate_text_tokens(text)),
            CompactionHistoryItemKind::ToolExchange {
                call,
                result,
                content,
                call_prompt_projection,
                prompt_projection,
            } => {
                match (*call_prompt_projection, *prompt_projection) {
                    (ToolCallPromptProjection::Hidden, ToolResultPromptProjection::Hidden) => {
                        return Ok(0);
                    }
                    (ToolCallPromptProjection::Full, ToolResultPromptProjection::Full)
                    | (
                        ToolCallPromptProjection::Full,
                        ToolResultPromptProjection::ArtifactNotice,
                    ) => {}
                    (ToolCallPromptProjection::Hidden, _)
                    | (ToolCallPromptProjection::Full, ToolResultPromptProjection::Hidden) => {
                        return Err(CompactionError::StaleWindow.into());
                    }
                }
                let arguments =
                    serde_json::to_string(call.arguments().as_object()).map_err(|error| {
                        CompactionError::PayloadSerialization {
                            message: error.to_string(),
                        }
                    })?;
                let result_text = if *prompt_projection
                    == ToolResultPromptProjection::ArtifactNotice
                    || archived_tool_call_ids.contains(call.id())
                {
                    archived_tool_result_notice_json(
                        TranscriptItemId::new(self.history_id),
                        result.status(),
                        result.artifact().id(),
                    )
                } else {
                    exact_artifact_text(content)?.1.to_owned()
                };
                Ok(estimate_text_tokens(call.name().as_str())
                    + estimate_text_tokens(&arguments)
                    + estimate_text_tokens(&result_text))
            }
        }
    }

    pub(super) fn tool_result_archive_candidate(
        &self,
    ) -> Option<(u64, merry_core::ToolCallId, bool)> {
        match &self.kind {
            CompactionHistoryItemKind::ToolExchange {
                call,
                call_prompt_projection,
                prompt_projection,
                ..
            } => match (*call_prompt_projection, *prompt_projection) {
                (ToolCallPromptProjection::Full, ToolResultPromptProjection::Full) => {
                    Some((self.history_id, call.id().clone(), false))
                }
                (ToolCallPromptProjection::Full, ToolResultPromptProjection::ArtifactNotice) => {
                    Some((self.history_id, call.id().clone(), true))
                }
                (ToolCallPromptProjection::Hidden, ToolResultPromptProjection::Hidden)
                | (ToolCallPromptProjection::Hidden, _)
                | (ToolCallPromptProjection::Full, ToolResultPromptProjection::Hidden) => None,
            },
            CompactionHistoryItemKind::User { .. }
            | CompactionHistoryItemKind::Assistant { .. } => None,
        }
    }
}

pub(super) fn permission_review_context_entry(
    item: &CompactionHistoryItem,
) -> PermissionReviewContextEntry {
    match &item.kind {
        CompactionHistoryItemKind::User { text } => PermissionReviewContextEntry::new(
            "user",
            crate::compaction::bounded_excerpt(text, PERMISSION_REVIEW_ENTRY_MAX_BYTES),
        ),
        CompactionHistoryItemKind::Assistant { text } => PermissionReviewContextEntry::new(
            "assistant",
            crate::compaction::bounded_excerpt(text, PERMISSION_REVIEW_ENTRY_MAX_BYTES),
        ),
        CompactionHistoryItemKind::ToolExchange {
            call,
            result,
            content,
            ..
        } => {
            let arguments_json = serde_json::to_string(call.arguments().as_object())
                .unwrap_or_else(|_| "<unserializable arguments>".to_owned());
            let text = format!(
                "tool_call:{} arguments:{} result_status:{} artifact:{} content:{}",
                call.name(),
                arguments_json,
                tool_call_result_status_label(result.status()),
                result.artifact().id(),
                artifact_content_preview(content, PERMISSION_REVIEW_ENTRY_MAX_BYTES),
            );
            PermissionReviewContextEntry::new(
                "tool",
                crate::compaction::bounded_excerpt(&text, PERMISSION_REVIEW_ENTRY_MAX_BYTES),
            )
        }
    }
}

fn exact_artifact_text(content: &ArtifactContent) -> Result<(&'static str, &str), RuntimeError> {
    match content {
        ArtifactContent::Text { content } => Ok(("text", content)),
        ArtifactContent::Json { content } => Ok(("json", content)),
        ArtifactContent::Binary { .. }
        | ArtifactContent::Image { .. }
        | ArtifactContent::Other { .. } => Err(CompactionError::PayloadSerialization {
            message: "compaction tool result content must be text or json".to_owned(),
        }
        .into()),
    }
}

fn artifact_content_preview(content: &ArtifactContent, max_bytes: usize) -> String {
    match content.as_text() {
        Some(text) => crate::compaction::bounded_excerpt(text, max_bytes),
        None => format!(
            "{:?} content, {} bytes",
            content.kind(),
            content.as_bytes().len()
        ),
    }
}

fn tool_call_result_status_label(status: merry_core::ToolCallResultStatus) -> &'static str {
    match status {
        merry_core::ToolCallResultStatus::Succeeded => "succeeded",
        merry_core::ToolCallResultStatus::Failed => "failed",
    }
}
