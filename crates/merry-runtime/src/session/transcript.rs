use super::SessionState;
use crate::{
    RuntimeError,
    artifact::{ArtifactContent, ArtifactError},
};
use merry_core::{ArtifactId, PendingToolCall, ToolCallId, ToolCallResult};
#[cfg(test)]
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TranscriptItemId(u64);

impl TranscriptItemId {
    #[must_use]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub(crate) const fn as_u64(self) -> u64 {
        self.0
    }

    fn checked_next(self) -> Result<Self, RuntimeError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(RuntimeError::TranscriptItemIdExhausted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserInputOrigin {
    ExternalUser,
    #[allow(dead_code)]
    RuntimeControl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TranscriptItem {
    UserMessage {
        id: TranscriptItemId,
        text: String,
        origin: UserInputOrigin,
    },
    AssistantText {
        id: TranscriptItemId,
        artifact_id: ArtifactId,
    },
    ToolCall {
        id: TranscriptItemId,
        call: PendingToolCall,
    },
    ToolResult {
        id: TranscriptItemId,
        call_id: ToolCallId,
        result: ToolCallResult,
        artifact_id: ArtifactId,
    },
}

impl TranscriptItem {
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn id(&self) -> TranscriptItemId {
        match self {
            Self::UserMessage { id, .. }
            | Self::AssistantText { id, .. }
            | Self::ToolCall { id, .. }
            | Self::ToolResult { id, .. } => *id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TranscriptItemSnapshot {
    UserMessage {
        text: String,
        origin: UserInputOrigin,
    },
    AssistantText {
        text: String,
    },
    ToolCall {
        call: PendingToolCall,
    },
    ToolResult {
        call_id: ToolCallId,
        result: ToolCallResult,
        content: ArtifactContent,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Transcript {
    items: Vec<TranscriptItem>,
    next_id: TranscriptItemId,
}

impl Transcript {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            items: Vec::new(),
            next_id: TranscriptItemId::new(0),
        }
    }

    #[must_use]
    pub(crate) fn items(&self) -> &[TranscriptItem] {
        &self.items
    }

    #[must_use]
    pub(crate) const fn next_id(&self) -> TranscriptItemId {
        self.next_id
    }

    pub(crate) fn push_user_message(
        &mut self,
        text: impl Into<String>,
        origin: UserInputOrigin,
    ) -> Result<TranscriptItemId, RuntimeError> {
        let id = self.allocate_id()?;
        self.items.push(TranscriptItem::UserMessage {
            id,
            text: text.into(),
            origin,
        });
        Ok(id)
    }

    pub(crate) fn push_assistant_text(
        &mut self,
        artifact_id: ArtifactId,
    ) -> Result<TranscriptItemId, RuntimeError> {
        let id = self.allocate_id()?;
        self.items
            .push(TranscriptItem::AssistantText { id, artifact_id });
        Ok(id)
    }

    pub(crate) fn push_tool_call(
        &mut self,
        call: PendingToolCall,
    ) -> Result<TranscriptItemId, RuntimeError> {
        let id = self.allocate_id()?;
        self.items.push(TranscriptItem::ToolCall { id, call });
        Ok(id)
    }

    pub(crate) fn push_tool_result(
        &mut self,
        call_id: ToolCallId,
        result: ToolCallResult,
        artifact_id: ArtifactId,
    ) -> Result<TranscriptItemId, RuntimeError> {
        let id = self.allocate_id()?;
        self.items.push(TranscriptItem::ToolResult {
            id,
            call_id,
            result,
            artifact_id,
        });
        Ok(id)
    }

    pub(crate) fn remove_tool_call(&mut self, call_id: &ToolCallId) -> bool {
        let Some(index) = self.items.iter().position(
            |item| matches!(item, TranscriptItem::ToolCall { call, .. } if call.id() == call_id),
        ) else {
            return false;
        };

        self.items.remove(index);
        true
    }

    #[cfg(test)]
    pub(crate) fn retain_ids(&mut self, retained: BTreeSet<TranscriptItemId>) {
        self.items.retain(|item| retained.contains(&item.id()));
    }

    pub(crate) fn remove_compacted_history_prefix(&mut self, covered_history_item_count: usize) {
        if covered_history_item_count == 0 {
            return;
        }

        let mut covered_items = 0usize;
        let mut remove_count = 0usize;
        for item in &self.items {
            let remove = match item {
                TranscriptItem::UserMessage { .. } | TranscriptItem::AssistantText { .. } => {
                    if covered_items < covered_history_item_count {
                        covered_items += 1;
                        true
                    } else {
                        false
                    }
                }
                TranscriptItem::ToolCall { .. } => covered_items < covered_history_item_count,
                TranscriptItem::ToolResult { .. } => {
                    if covered_items < covered_history_item_count {
                        covered_items += 1;
                        true
                    } else {
                        false
                    }
                }
            };

            if !remove {
                break;
            }
            remove_count += 1;
        }

        if remove_count > 0 {
            self.items.drain(..remove_count);
        }
    }

    fn allocate_id(&mut self) -> Result<TranscriptItemId, RuntimeError> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_next()?;
        Ok(id)
    }
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    pub(crate) fn transcript_snapshot(&self) -> Result<Vec<TranscriptItemSnapshot>, ArtifactError> {
        self.transcript
            .items()
            .iter()
            .map(|item| match item {
                TranscriptItem::UserMessage { text, origin, .. } => {
                    Ok(TranscriptItemSnapshot::UserMessage {
                        text: text.clone(),
                        origin: *origin,
                    })
                }
                TranscriptItem::AssistantText { artifact_id, .. } => {
                    let content = self.read_artifact_content(artifact_id)?;
                    let text =
                        content
                            .as_text()
                            .ok_or_else(|| ArtifactError::InvalidEvidenceLocator {
                                id: artifact_id.clone(),
                                reason: "assistant transcript artifact is not textual",
                            })?;
                    Ok(TranscriptItemSnapshot::AssistantText {
                        text: text.to_owned(),
                    })
                }
                TranscriptItem::ToolCall { call, .. } => {
                    Ok(TranscriptItemSnapshot::ToolCall { call: call.clone() })
                }
                TranscriptItem::ToolResult {
                    call_id,
                    result,
                    artifact_id,
                    ..
                } => {
                    let content = self.read_artifact_content(artifact_id)?;
                    Ok(TranscriptItemSnapshot::ToolResult {
                        call_id: call_id.clone(),
                        result: result.clone(),
                        content,
                    })
                }
            })
            .collect()
    }
}
