//! Public tool output extraction for artifact-backed tool results.

use crate::ArtifactContent;
use merry_core::ToolOutput;

#[must_use]
pub(crate) fn public_tool_output(content: &ArtifactContent) -> Option<ToolOutput> {
    match content {
        ArtifactContent::Text(text) => Some(ToolOutput::Text { text: text.clone() }),
        ArtifactContent::Json(json) => Some(ToolOutput::Json { json: json.clone() }),
        ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => None,
    }
}
