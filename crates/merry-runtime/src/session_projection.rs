use crate::{ArtifactContent, session::TranscriptItemSnapshot};
use merry_core::{ArtifactRef, PendingToolCall, ToolCallId, ToolCallResult, ToolOutput};

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
