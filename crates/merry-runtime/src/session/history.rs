use crate::{
    RuntimeError,
    compaction::{CitationCompactionToolResult, CitationCompactionTurnItem, CompactionError},
    permission::PermissionReviewContextEntry,
    token_estimate::{BYTES_PER_TOKEN, estimate_text_tokens},
};
use merry_core::{PendingToolCall, ToolCallResult, ToolOutput, ToolOutputKind};
use std::{borrow::Cow, collections::BTreeSet};

use super::transcript::{
    ToolCallPromptProjection, ToolResultPromptProjection, collapsed_tool_result_notice_json,
};

const PERMISSION_REVIEW_ENTRY_MAX_BYTES: usize = 2048;
const PERMISSION_REVIEW_TOOL_ARGS_MAX_BYTES: usize = 512;
const PERMISSION_REVIEW_SUCCEEDED_TOOL_CONTENT_MAX_BYTES: usize = 256;
const PERMISSION_REVIEW_FAILED_TOOL_CONTENT_MAX_BYTES: usize = 1024;

/// Fixed JSON keys, tags, ids, and separators one payload item adds beyond its text.
///
/// Measured against the serialized payload: a small user item carries about 60
/// bytes beyond its text, and a tool exchange about 160 because it also names the
/// call, the artifact, the result status, and the content kind. These constants
/// are upper bounds, because an underestimated envelope makes the planner believe
/// a larger covered window fits than the runtime can actually measure.
const COMPACTION_PAYLOAD_ITEM_ENVELOPE_BYTES: u64 = 64;
const COMPACTION_PAYLOAD_TOOL_ITEM_ENVELOPE_BYTES: u64 = 192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompactionHistoryItem<'a> {
    pub(super) history_id: u64,
    pub(super) kind: CompactionHistoryItemKind<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CompactionHistoryItemKind<'a> {
    User {
        text: &'a str,
    },
    Assistant {
        text: &'a str,
    },
    ToolExchange {
        call: &'a PendingToolCall,
        result: &'a ToolCallResult,
        content: &'a ToolOutput,
        model_content: &'a ToolOutput,
        call_prompt_projection: ToolCallPromptProjection,
        prompt_projection: ToolResultPromptProjection,
    },
}

impl<'a> CompactionHistoryItem<'a> {
    /// Returns whether this item is one user message.
    pub(super) const fn is_user(&self) -> bool {
        matches!(self.kind, CompactionHistoryItemKind::User { .. })
    }

    /// Returns whether this item is one tool exchange, call and result together.
    pub(super) const fn is_tool_exchange(&self) -> bool {
        matches!(self.kind, CompactionHistoryItemKind::ToolExchange { .. })
    }

    pub(super) fn user(history_id: u64, text: &'a str) -> Self {
        Self {
            history_id,
            kind: CompactionHistoryItemKind::User { text },
        }
    }

    pub(super) fn assistant(history_id: u64, text: &'a str) -> Self {
        Self {
            history_id,
            kind: CompactionHistoryItemKind::Assistant { text },
        }
    }

    pub(super) fn tool_exchange(
        history_id: u64,
        call: &'a PendingToolCall,
        result: &'a ToolCallResult,
        content: &'a ToolOutput,
        call_prompt_projection: ToolCallPromptProjection,
        prompt_projection: ToolResultPromptProjection,
        model_content: Option<&'a ToolOutput>,
    ) -> Self {
        Self {
            history_id,
            kind: CompactionHistoryItemKind::ToolExchange {
                call,
                result,
                content,
                model_content: model_content.unwrap_or(content),
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
            CompactionHistoryItemKind::User { text } => CitationCompactionTurnItem::user(
                self.history_id,
                ref_id.to_owned(),
                (*text).to_owned(),
            ),
            CompactionHistoryItemKind::Assistant { text } => CitationCompactionTurnItem::assistant(
                self.history_id,
                ref_id.to_owned(),
                (*text).to_owned(),
            ),
            CompactionHistoryItemKind::ToolExchange {
                call,
                result,
                model_content,
                prompt_projection,
                ..
            } => {
                // The request already shortened this result, so the payload carries the
                // same notice instead of the collapsed body.
                let use_notice = *prompt_projection == ToolResultPromptProjection::Collapsed;
                let (content_kind, content) =
                    compaction_tool_result_text(result, model_content, use_notice);
                CitationCompactionTurnItem::tool_exchange(
                    self.history_id,
                    ref_id.to_owned(),
                    call.id(),
                    call.name().as_str().to_owned(),
                    serde_json::Value::Object(call.arguments().as_object().clone()),
                    CitationCompactionToolResult::new(
                        result.status(),
                        content_kind,
                        content.into_owned(),
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
                model_content,
                call_prompt_projection,
                prompt_projection,
                ..
            } => {
                match (*call_prompt_projection, *prompt_projection) {
                    (ToolCallPromptProjection::Hidden, ToolResultPromptProjection::Hidden) => {
                        return Ok(0);
                    }
                    (ToolCallPromptProjection::Full, ToolResultPromptProjection::Full)
                    | (ToolCallPromptProjection::Full, ToolResultPromptProjection::Collapsed) => {}
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
                let (_, result_text) = compaction_tool_result_text(
                    result,
                    model_content,
                    *prompt_projection == ToolResultPromptProjection::Collapsed
                        || archived_tool_call_ids.contains(call.id()),
                );
                Ok(estimate_text_tokens(call.name().as_str())
                    + estimate_text_tokens(&arguments)
                    + estimate_text_tokens(&result_text))
            }
        }
    }

    /// Estimated tokens this item contributes to the compaction payload.
    ///
    /// Covered turns travel through the payload with the text the request itself
    /// shows, so an archived tool result contributes its artifact notice rather
    /// than the body the artifact holds. Window planning uses this estimate to cap
    /// how much history one compaction request reads.
    ///
    /// This estimate is not the authority. Text is measured from its raw byte
    /// length while the payload serializes it with JSON escaping, so content with
    /// many newlines can add up to one extra byte per escaped character, and the
    /// envelope constants only bound the fixed part. The authority is
    /// [`crate::compaction::CitationCompactionInput::covered_payload_token_estimate`],
    /// which measures the built payload; the runtime sizes a request from that
    /// value and re-plans when this estimate was too optimistic.
    pub(super) fn compaction_payload_token_estimate(&self) -> Result<u64, RuntimeError> {
        let (content_tokens, envelope_bytes) = match &self.kind {
            CompactionHistoryItemKind::User { text }
            | CompactionHistoryItemKind::Assistant { text } => (
                estimate_text_tokens(text),
                COMPACTION_PAYLOAD_ITEM_ENVELOPE_BYTES,
            ),
            CompactionHistoryItemKind::ToolExchange {
                call,
                result,
                model_content,
                prompt_projection,
                ..
            } => {
                let result_text = compaction_tool_result_text(
                    result,
                    model_content,
                    *prompt_projection == ToolResultPromptProjection::Collapsed,
                )
                .1;
                let arguments =
                    serde_json::to_string(call.arguments().as_object()).map_err(|error| {
                        RuntimeError::from(CompactionError::PayloadSerialization {
                            message: error.to_string(),
                        })
                    })?;
                (
                    estimate_text_tokens(call.name().as_str())
                        .saturating_add(estimate_text_tokens(&arguments))
                        .saturating_add(estimate_text_tokens(&result_text)),
                    COMPACTION_PAYLOAD_TOOL_ITEM_ENVELOPE_BYTES,
                )
            }
        };
        Ok(content_tokens.saturating_add(envelope_bytes.div_ceil(BYTES_PER_TOKEN)))
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
                (ToolCallPromptProjection::Full, ToolResultPromptProjection::Collapsed) => {
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

pub(super) fn permission_review_task_anchor_entry(objective: &str) -> PermissionReviewContextEntry {
    PermissionReviewContextEntry::new(
        "task_anchor",
        crate::compaction::bounded_excerpt(objective, PERMISSION_REVIEW_ENTRY_MAX_BYTES),
    )
}

pub(super) fn permission_review_context_entry(
    item: &CompactionHistoryItem<'_>,
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
            let arguments_preview = crate::compaction::bounded_excerpt(
                &arguments_json,
                PERMISSION_REVIEW_TOOL_ARGS_MAX_BYTES,
            );
            let content_limit = match result.status() {
                merry_core::ToolCallResultStatus::Failed => {
                    PERMISSION_REVIEW_FAILED_TOOL_CONTENT_MAX_BYTES
                }
                merry_core::ToolCallResultStatus::Succeeded => {
                    PERMISSION_REVIEW_SUCCEEDED_TOOL_CONTENT_MAX_BYTES
                }
            };
            let text = format!(
                "tool_call:{} arguments:{} result_status:{} content:{}",
                call.name(),
                arguments_preview,
                tool_call_result_status_label(result.status()),
                crate::compaction::bounded_excerpt(content.as_str(), content_limit),
            );
            PermissionReviewContextEntry::new(
                "tool",
                crate::compaction::bounded_excerpt(&text, PERMISSION_REVIEW_ENTRY_MAX_BYTES),
            )
        }
    }
}

/// Text the compaction payload carries for one tool result.
fn compaction_tool_result_text<'a>(
    result: &ToolCallResult,
    content: &'a ToolOutput,
    use_notice: bool,
) -> (&'static str, Cow<'a, str>) {
    if use_notice {
        (
            "json",
            Cow::Owned(collapsed_tool_result_notice_json(
                result.status(),
                result.call_id(),
            )),
        )
    } else {
        let content_kind = match content.kind() {
            ToolOutputKind::Text => "text",
            ToolOutputKind::Json => "json",
        };
        (content_kind, Cow::Borrowed(content.as_str()))
    }
}

fn tool_call_result_status_label(status: merry_core::ToolCallResultStatus) -> &'static str {
    match status {
        merry_core::ToolCallResultStatus::Succeeded => "succeeded",
        merry_core::ToolCallResultStatus::Failed => "failed",
    }
}
