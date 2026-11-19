use crate::session::TranscriptItemSnapshot;
use merry_core::{ImageSummary, PendingToolCall, ToolCallId, ToolCallResult, ToolOutput};
use std::collections::BTreeMap;

/// Read-only public projection of persisted session transcript items.
///
/// This is intended for UI/SDK resume views. It is not provider wire history and
/// does not mutate runtime state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTranscriptItem {
    UserMessage {
        text: String,
        images: Vec<ImageSummary>,
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
        output: Option<ToolOutput>,
    },
}

/// Complete transcript record used to rebuild the runtime trajectory after
/// restoring a persisted session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionTrajectoryItem {
    UserMessage {
        item_id: u64,
        model_turn_id: u64,
        text: String,
    },
    AssistantText {
        item_id: u64,
        model_turn_id: u64,
        text: String,
    },
    ToolCall {
        item_id: u64,
        model_turn_id: u64,
        call: PendingToolCall,
    },
    ToolResult {
        item_id: u64,
        model_turn_id: u64,
        call_id: ToolCallId,
        result: ToolCallResult,
        output: Option<ToolOutput>,
    },
}

/// Complete transcript evidence and durable model-turn sequence associations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionTrajectory {
    pub(crate) items: Vec<SessionTrajectoryItem>,
    pub(crate) model_turn_sequences: BTreeMap<u64, u64>,
}

impl From<TranscriptItemSnapshot> for SessionTranscriptItem {
    fn from(value: TranscriptItemSnapshot) -> Self {
        match value {
            TranscriptItemSnapshot::UserMessage { text, images, .. } => Self::UserMessage {
                text,
                images: images
                    .into_iter()
                    .map(|image| ImageSummary::new(image.label(), image.width(), image.height()))
                    .collect(),
            },
            TranscriptItemSnapshot::AssistantText { text } => Self::AssistantText { text },
            TranscriptItemSnapshot::ToolCall { call } => Self::ToolCall { call },
            TranscriptItemSnapshot::ToolResult {
                call_id,
                result,
                output,
                ..
            } => Self::ToolResult {
                call_id,
                result,
                output: Some(output),
            },
        }
    }
}
