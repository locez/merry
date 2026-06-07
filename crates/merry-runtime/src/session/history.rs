use crate::{
    RuntimeError,
    artifact::ArtifactContent,
    compaction::{
        CitationCompactionPolicy, CitationCompactionToolCall, CitationCompactionToolResult,
        CitationCompactionWindowItem, CompactionError,
    },
    permission::PermissionReviewContextEntry,
};
use merry_core::{ArtifactId, PendingToolCall, ToolCallResult};

const PERMISSION_REVIEW_ENTRY_MAX_BYTES: usize = 2048;

/// Resolved tool call state that has not yet been compiled into a provider request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedToolContinuation {
    pub(super) history_id: u64,
    pub(super) call: PendingToolCall,
    pub(super) result: ToolCallResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionMessage {
    User {
        history_id: u64,
        text: String,
    },
    Assistant {
        history_id: u64,
        artifact_id: ArtifactId,
    },
}

impl ResolvedToolContinuation {
    pub(super) fn new(history_id: u64, call: PendingToolCall, result: ToolCallResult) -> Self {
        Self {
            history_id,
            call,
            result,
        }
    }
}

impl SessionMessage {
    pub(super) fn user(history_id: u64, text: String) -> Self {
        Self::User { history_id, text }
    }

    pub(super) fn assistant(history_id: u64, artifact_id: ArtifactId) -> Self {
        Self::Assistant {
            history_id,
            artifact_id,
        }
    }

    pub(super) fn history_id(&self) -> u64 {
        match self {
            Self::User { history_id, .. } | Self::Assistant { history_id, .. } => *history_id,
        }
    }
}

/// Tool continuation data read from session state for one request compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolContinuationSnapshot {
    call: PendingToolCall,
    result: ToolCallResult,
    content: ArtifactContent,
}

impl ResolvedToolContinuationSnapshot {
    pub(super) fn new(
        call: PendingToolCall,
        result: ToolCallResult,
        content: ArtifactContent,
    ) -> Self {
        Self {
            call,
            result,
            content,
        }
    }

    pub(crate) fn call(&self) -> &PendingToolCall {
        &self.call
    }

    pub(crate) fn result(&self) -> &ToolCallResult {
        &self.result
    }

    pub(crate) fn content(&self) -> &ArtifactContent {
        &self.content
    }
}

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
    ) -> Self {
        Self {
            history_id,
            kind: CompactionHistoryItemKind::ToolExchange {
                call: Box::new(call),
                result: Box::new(result),
                content: Box::new(content),
            },
        }
    }

    pub(super) fn to_compaction_window_item(
        &self,
        ref_id: &str,
        policy: CitationCompactionPolicy,
    ) -> Result<CitationCompactionWindowItem, RuntimeError> {
        let item = match &self.kind {
            CompactionHistoryItemKind::User { text } => CitationCompactionWindowItem::user(
                self.history_id,
                ref_id.to_owned(),
                crate::compaction::bounded_excerpt(text, policy.max_ref_excerpt_bytes()),
            ),
            CompactionHistoryItemKind::Assistant { text } => {
                CitationCompactionWindowItem::assistant(
                    self.history_id,
                    ref_id.to_owned(),
                    crate::compaction::bounded_excerpt(text, policy.max_ref_excerpt_bytes()),
                )
            }
            CompactionHistoryItemKind::ToolExchange {
                call,
                result,
                content,
            } => {
                let arguments_json =
                    serde_json::to_string(call.arguments().as_object()).map_err(|error| {
                        CompactionError::PayloadSerialization {
                            message: error.to_string(),
                        }
                    })?;
                let excerpt = format!(
                    "tool_call:{}\narguments:{}\nresult_status:{}\nartifact:{}\ncontent:{}",
                    call.name(),
                    arguments_json,
                    tool_call_result_status_label(result.status()),
                    result.artifact().id(),
                    artifact_content_preview(content, policy.max_ref_excerpt_bytes())
                );
                CitationCompactionWindowItem::tool_exchange(
                    self.history_id,
                    ref_id.to_owned(),
                    crate::compaction::bounded_excerpt(&excerpt, policy.max_ref_excerpt_bytes()),
                    CitationCompactionToolCall::new(
                        call.name().as_str().to_owned(),
                        arguments_json,
                    ),
                    CitationCompactionToolResult::new(result.status(), result.artifact().id()),
                )
            }
        };

        Ok(item)
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
