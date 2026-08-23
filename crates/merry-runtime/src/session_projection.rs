use crate::{ArtifactContent, session::TranscriptItemSnapshot};
use merry_core::{ArtifactRef, PendingToolCall, ToolCallId, ToolCallResult, ToolOutput};
use std::collections::BTreeMap;

/// Read-only public projection of persisted session transcript items.
///
/// This is intended for UI/SDK resume views. It is not provider wire history and
/// does not mutate runtime state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTranscriptItem {
    UserMessage {
        text: String,
        images: Vec<ArtifactRef>,
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

/// Complete transcript evidence used to rebuild the runtime trajectory after
/// restoring a persisted session.
///
/// This stays internal to the runtime. The public transcript projection keeps
/// its smaller compatibility surface, while trajectory replay retains the
/// artifact identities needed to recover exact sequence and evidence links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionTrajectoryItem {
    UserMessage {
        item_id: u64,
        model_turn_id: u64,
        artifact: ArtifactRef,
        text: String,
    },
    AssistantText {
        item_id: u64,
        model_turn_id: u64,
        artifact: ArtifactRef,
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
        artifact: ArtifactRef,
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
                    .map(|image| image.artifact().clone())
                    .collect(),
            },
            TranscriptItemSnapshot::AssistantText { text } => Self::AssistantText { text },
            TranscriptItemSnapshot::ToolCall { call } => Self::ToolCall { call },
            TranscriptItemSnapshot::ToolResult {
                call_id,
                result,
                content,
            } => Self::ToolResult {
                call_id,
                result,
                output: tool_output_from_content(content),
            },
        }
    }
}

fn tool_output_from_content(content: ArtifactContent) -> Option<ToolOutput> {
    match content {
        ArtifactContent::Text { content: text } => Some(ToolOutput::Text { text }),
        ArtifactContent::Json { content: json } => Some(ToolOutput::Json { json }),
        ArtifactContent::Binary { .. }
        | ArtifactContent::Image { .. }
        | ArtifactContent::Other { .. } => None,
    }
}
