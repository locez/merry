//! Public tool output extraction for artifact-backed tool results.

use crate::ArtifactContent;
use merry_core::ToolOutput;

#[must_use]
pub(crate) fn public_tool_output(content: &ArtifactContent) -> Option<ToolOutput> {
    match content {
        ArtifactContent::Text { content: text } => Some(ToolOutput::Text { text: text.clone() }),
        ArtifactContent::Json { content: json } => Some(ToolOutput::Json { json: json.clone() }),
        ArtifactContent::Binary { .. }
        | ArtifactContent::Image { .. }
        | ArtifactContent::Other { .. } => None,
    }
}
