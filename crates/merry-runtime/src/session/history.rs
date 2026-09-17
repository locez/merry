use crate::{
    RuntimeError,
    artifact::ArtifactContent,
    compaction::{CitationCompactionToolResult, CitationCompactionTurnItem, CompactionError},
    permission::PermissionReviewContextEntry,
    token_estimate::{BYTES_PER_TOKEN, estimate_text_tokens},
};
use merry_core::{PendingToolCall, ToolCallResult};
use std::collections::BTreeSet;

use super::transcript::{
    ToolCallPromptProjection, ToolResultPromptProjection, TranscriptItemId,
    archived_tool_result_notice_json,
};

const PERMISSION_REVIEW_ENTRY_MAX_BYTES: usize = 2048;

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
    /// Returns whether this item is one tool exchange, call and result together.
    pub(super) const fn is_tool_exchange(&self) -> bool {
        matches!(self.kind, CompactionHistoryItemKind::ToolExchange { .. })
    }

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
        keep_tool_result_full: bool,
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
                prompt_projection,
                ..
            } => {
                // The request already shortened this result, or a one-shot pass
                // shortened it to make the payload fit.
                let use_notice = !keep_tool_result_full
                    || *prompt_projection == ToolResultPromptProjection::ArtifactNotice;
                let (content_kind, content) =
                    compaction_tool_result_text(self.history_id, result, content, use_notice)?;
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
                        content,
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
                content,
                prompt_projection,
                ..
            } => {
                let result_text = compaction_tool_result_text(
                    self.history_id,
                    result,
                    content,
                    *prompt_projection == ToolResultPromptProjection::ArtifactNotice,
                )?
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

/// Text the compaction payload carries for one tool result.
///
/// The payload shows the same result text the request shows. When the request
/// replaced an archived result with an artifact notice, the payload carries that
/// notice: sending the archived body instead would ask the compactor to read
/// content the model never saw, and the checkpoint only summarizes what the
/// conversation actually held. The notice still names the artifact, so the
/// checkpoint can cite the ref and read the body later.
///
/// Both the payload builder and the sizing estimate go through this one function.
/// They disagreed once, and the runtime then believed a covered window fit while
/// the payload it built did not, which ended the step with "no compaction window
/// fits the compaction request budget".
fn compaction_tool_result_text(
    history_id: u64,
    result: &ToolCallResult,
    content: &ArtifactContent,
    use_notice: bool,
) -> Result<(&'static str, String), RuntimeError> {
    if use_notice {
        Ok((
            "json",
            archived_tool_result_notice_json(
                TranscriptItemId::new(history_id),
                result.status(),
                result.artifact().id(),
            ),
        ))
    } else {
        let (content_kind, content) = exact_artifact_text(content)?;
        Ok((content_kind, content.to_owned()))
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
