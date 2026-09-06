//! Validated evidence ranges and exact bounded reads from stored payloads.

use crate::artifact::{ArtifactError, content::ArtifactContent};
use merry_core::{ArtifactId, EvidenceLocator};

/// One bounded UTF-8 page read from an exact evidence reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEvidencePage {
    pub(super) artifact_id: ArtifactId,
    pub(super) content: String,
    pub(super) offset: usize,
    pub(super) next_offset: Option<usize>,
    pub(super) total_bytes: usize,
}

impl TextEvidencePage {
    /// Borrows the source artifact identifier.
    #[must_use]
    pub fn artifact_id(&self) -> &ArtifactId {
        &self.artifact_id
    }

    /// Borrows the exact page content.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns this page's byte offset inside the selected evidence range.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the next byte offset, or `None` when this page reaches the end.
    #[must_use]
    pub fn next_offset(&self) -> Option<usize> {
        self.next_offset
    }

    /// Returns the selected evidence range length in bytes.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

pub(super) fn text_evidence_bounds(
    artifact_id: &ArtifactId,
    content: &ArtifactContent,
    locator: &EvidenceLocator,
) -> Result<(usize, usize), ArtifactError> {
    let text = content
        .as_text()
        .ok_or_else(|| ArtifactError::NonTextEvidencePage {
            id: artifact_id.clone(),
        })?;
    if locator.is_whole_artifact() {
        return Ok((0, text.len()));
    }

    if let Some((start, end)) = locator.as_line_range() {
        line_range_bounds(text, start, end)
            .ok_or_else(|| invalid_locator(artifact_id, "line range is outside artifact content"))
    } else if let Some((start, end)) = locator.as_byte_range() {
        let bounds = byte_range_bounds(artifact_id, content, start, end)?;
        Ok((bounds.start, bounds.end))
    } else if locator.as_json_pointer().is_some() {
        Err(ArtifactError::UnsupportedEvidenceLocator {
            id: artifact_id.clone(),
            locator_kind: "json_pointer",
        })
    } else if locator.as_named_section().is_some() {
        Err(ArtifactError::UnsupportedEvidenceLocator {
            id: artifact_id.clone(),
            locator_kind: "named_section",
        })
    } else {
        Err(invalid_locator(artifact_id, "unknown evidence locator"))
    }
}

pub(super) fn validate_locator(
    artifact_id: &ArtifactId,
    content: &ArtifactContent,
    locator: &EvidenceLocator,
) -> Result<(), ArtifactError> {
    if locator.is_whole_artifact() {
        return Ok(());
    }

    if let Some((start, end)) = locator.as_line_range() {
        let text = content.as_text().ok_or_else(|| {
            invalid_locator(artifact_id, "line range requires textual artifact content")
        })?;
        line_range_bounds(text, start, end)
            .map(|_| ())
            .ok_or_else(|| invalid_locator(artifact_id, "line range is outside artifact content"))
    } else if let Some((start, end)) = locator.as_byte_range() {
        byte_range_bounds(artifact_id, content, start, end).map(|_| ())
    } else if locator.as_json_pointer().is_some() {
        Err(ArtifactError::UnsupportedEvidenceLocator {
            id: artifact_id.clone(),
            locator_kind: "json_pointer",
        })
    } else if locator.as_named_section().is_some() {
        Err(ArtifactError::UnsupportedEvidenceLocator {
            id: artifact_id.clone(),
            locator_kind: "named_section",
        })
    } else {
        Err(invalid_locator(artifact_id, "unknown evidence locator"))
    }
}

fn byte_range_bounds(
    artifact_id: &ArtifactId,
    content: &ArtifactContent,
    start: u64,
    end: u64,
) -> Result<std::ops::Range<usize>, ArtifactError> {
    let start = usize::try_from(start)
        .map_err(|_| invalid_locator(artifact_id, "byte range start is outside platform bounds"))?;
    let end = usize::try_from(end)
        .map_err(|_| invalid_locator(artifact_id, "byte range end is outside platform bounds"))?;
    if start > end || end > content.as_bytes().len() {
        return Err(invalid_locator(
            artifact_id,
            "byte range is outside artifact content",
        ));
    }
    if let Some(text) = content.as_text()
        && text.get(start..end).is_none()
    {
        return Err(invalid_locator(
            artifact_id,
            "byte range must align to utf-8 character boundaries for textual content",
        ));
    }
    Ok(start..end)
}

pub(super) fn read_located_content(
    artifact_id: &ArtifactId,
    content: &ArtifactContent,
    locator: &EvidenceLocator,
) -> Result<ArtifactContent, ArtifactError> {
    if locator.is_whole_artifact() {
        return Ok(content.clone());
    }

    if let Some((start, end)) = locator.as_line_range() {
        let text = content.as_text().ok_or_else(|| {
            invalid_locator(artifact_id, "line range requires textual artifact content")
        })?;
        let Some((start, end)) = line_range_bounds(text, start, end) else {
            return Err(invalid_locator(
                artifact_id,
                "line range is outside artifact content",
            ));
        };
        Ok(ArtifactContent::Text {
            content: text[start..end].to_owned(),
        })
    } else if let Some((start, end)) = locator.as_byte_range() {
        read_byte_range(artifact_id, content, start, end)
    } else if locator.as_json_pointer().is_some() {
        Err(ArtifactError::UnsupportedEvidenceLocator {
            id: artifact_id.clone(),
            locator_kind: "json_pointer",
        })
    } else if locator.as_named_section().is_some() {
        Err(ArtifactError::UnsupportedEvidenceLocator {
            id: artifact_id.clone(),
            locator_kind: "named_section",
        })
    } else {
        Err(invalid_locator(artifact_id, "unknown evidence locator"))
    }
}

pub(super) fn read_byte_range(
    artifact_id: &ArtifactId,
    content: &ArtifactContent,
    start: u64,
    end: u64,
) -> Result<ArtifactContent, ArtifactError> {
    let std::ops::Range { start, end } = byte_range_bounds(artifact_id, content, start, end)?;

    match content {
        ArtifactContent::Text { content: text } | ArtifactContent::Json { content: text } => {
            Ok(ArtifactContent::Text {
                content: text[start..end].to_owned(),
            })
        }
        ArtifactContent::Binary { bytes } => Ok(ArtifactContent::Binary {
            bytes: bytes[start..end].to_vec(),
        }),
        ArtifactContent::Image { bytes, .. } => {
            Ok(ArtifactContent::image(bytes[start..end].to_vec()))
        }
        ArtifactContent::Other { bytes } => Ok(ArtifactContent::Other {
            bytes: bytes[start..end].to_vec(),
        }),
    }
}

pub(super) fn line_range_bounds(text: &str, start: u64, end: u64) -> Option<(usize, usize)> {
    debug_assert!(start <= end);

    let mut line_start = 0_usize;
    let mut selected_start = None;

    for (line_number, segment) in (1_u64..).zip(text.split_inclusive('\n')) {
        let line_end = line_start + segment.len();
        if line_number == start {
            selected_start = Some(line_start);
        }

        if line_number == end {
            let content_end = line_content_end(text.as_bytes(), line_start, line_end);
            return selected_start.map(|start| (start, content_end));
        }

        line_start = line_end;
    }

    None
}

pub(super) fn line_content_end(bytes: &[u8], line_start: usize, line_end: usize) -> usize {
    let mut content_end = line_end;
    if content_end > line_start && bytes[content_end - 1] == b'\n' {
        content_end -= 1;
        if content_end > line_start && bytes[content_end - 1] == b'\r' {
            content_end -= 1;
        }
    }
    content_end
}

pub(super) fn invalid_locator(artifact_id: &ArtifactId, reason: &'static str) -> ArtifactError {
    ArtifactError::InvalidEvidenceLocator {
        id: artifact_id.clone(),
        reason,
    }
}

pub(super) fn invalid_page(artifact_id: &ArtifactId, reason: &'static str) -> ArtifactError {
    ArtifactError::InvalidEvidencePage {
        id: artifact_id.clone(),
        reason,
    }
}
