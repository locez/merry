//! In-memory artifact registry.

use merry_core::{ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef};
use std::{collections::BTreeMap, sync::Arc};
use thiserror::Error;

/// Exact content stored for an artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactContent {
    /// UTF-8 text.
    Text(String),
    /// Serialized JSON text.
    Json(String),
    /// Opaque binary bytes.
    Binary(Vec<u8>),
    /// Image bytes.
    Image(Vec<u8>),
    /// Provider-neutral bytes for artifact kinds not covered by stable variants.
    Other(Vec<u8>),
}

impl ArtifactContent {
    /// Creates UTF-8 text artifact content.
    pub fn text(content: impl Into<String>) -> Self {
        Self::Text(content.into())
    }

    /// Creates serialized JSON artifact content.
    pub fn json(content: impl Into<String>) -> Self {
        Self::Json(content.into())
    }

    /// Creates opaque binary artifact content.
    pub fn binary(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Binary(bytes.into())
    }

    /// Creates image artifact content.
    pub fn image(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Image(bytes.into())
    }

    /// Creates content for provider-neutral artifact kinds not covered by stable variants.
    pub fn other(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Other(bytes.into())
    }

    /// Returns the content kind.
    #[must_use]
    pub fn kind(&self) -> ArtifactContentKind {
        match self {
            Self::Text(_) => ArtifactContentKind::Text,
            Self::Json(_) => ArtifactContentKind::Json,
            Self::Binary(_) => ArtifactContentKind::Binary,
            Self::Image(_) => ArtifactContentKind::Image,
            Self::Other(_) => ArtifactContentKind::Other,
        }
    }

    /// Borrows textual artifact content.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(content) | Self::Json(content) => Some(content),
            Self::Binary(_) | Self::Image(_) | Self::Other(_) => None,
        }
    }

    /// Borrows artifact content as exact bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Text(content) | Self::Json(content) => content.as_bytes(),
            Self::Binary(bytes) | Self::Image(bytes) | Self::Other(bytes) => bytes,
        }
    }
}

/// Artifact content category used for metadata/content compatibility checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactContentKind {
    /// UTF-8 text.
    Text,
    /// Serialized JSON text.
    Json,
    /// Opaque binary bytes.
    Binary,
    /// Image bytes.
    Image,
    /// Provider-neutral bytes for artifact kinds not covered by stable variants.
    Other,
}

/// A recorded artifact reference and its exact content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    artifact: ArtifactRef,
    content: Arc<ArtifactContent>,
}

impl ArtifactRecord {
    /// Borrows the recorded artifact reference.
    #[must_use]
    pub fn artifact(&self) -> &ArtifactRef {
        &self.artifact
    }

    /// Borrows the recorded exact content.
    #[must_use]
    pub fn content(&self) -> &ArtifactContent {
        self.content.as_ref()
    }
}

/// Errors raised by artifact registry operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArtifactError {
    /// The artifact id already exists in this registry.
    #[error("artifact id {id} is already recorded")]
    DuplicateId {
        /// Duplicate artifact identifier.
        id: ArtifactId,
    },

    /// The requested artifact id is not recorded in this registry.
    #[error("artifact id {id} is not recorded")]
    MissingArtifact {
        /// Missing artifact identifier.
        id: ArtifactId,
    },

    /// Artifact metadata kind does not match the stored content kind.
    #[error(
        "artifact id {id} declares kind {artifact_kind:?}, but content kind is {content_kind:?}"
    )]
    IncompatibleContent {
        /// Artifact identifier.
        id: ArtifactId,
        /// Provider-neutral artifact kind declared in metadata.
        artifact_kind: ArtifactKind,
        /// Stored content category.
        content_kind: ArtifactContentKind,
    },

    /// The locator cannot reference exact content for the recorded artifact.
    #[error("artifact id {id} has invalid evidence locator: {reason}")]
    InvalidEvidenceLocator {
        /// Artifact identifier.
        id: ArtifactId,
        /// Actionable reason.
        reason: &'static str,
    },

    /// The locator type is not supported by the in-memory registry yet.
    #[error("artifact id {id} does not support {locator_kind} evidence locators yet")]
    UnsupportedEvidenceLocator {
        /// Artifact identifier.
        id: ArtifactId,
        /// Locator kind name.
        locator_kind: &'static str,
    },
}

/// In-memory artifact reference and content registry.
///
/// Recording returns an [`ArtifactRef`] only after metadata and exact content
/// have been stored, keeping state-before-reference usage natural for callers.
#[derive(Debug, Clone, Default)]
pub struct ArtifactRegistry {
    records: BTreeMap<ArtifactId, ArtifactRecord>,
}

impl ArtifactRegistry {
    /// Returns whether the registry has no recorded artifacts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Records an artifact reference and its exact content.
    pub fn record(
        &mut self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<ArtifactRef, ArtifactError> {
        let id = artifact.id().clone();
        if self.records.contains_key(&id) {
            return Err(ArtifactError::DuplicateId { id });
        }

        validate_content_kind(&artifact, &content)?;

        let recorded = artifact.clone();
        self.records.insert(
            id,
            ArtifactRecord {
                artifact,
                content: Arc::new(content),
            },
        );
        Ok(recorded)
    }

    /// Reads a recorded artifact by id.
    pub fn read_record(&self, id: &ArtifactId) -> Result<&ArtifactRecord, ArtifactError> {
        self.records
            .get(id)
            .ok_or_else(|| ArtifactError::MissingArtifact { id: id.clone() })
    }

    /// Reads a recorded artifact reference by id.
    pub fn read_ref(&self, id: &ArtifactId) -> Result<&ArtifactRef, ArtifactError> {
        self.read_record(id).map(ArtifactRecord::artifact)
    }

    /// Reads recorded exact content by artifact id.
    pub fn read_content(&self, id: &ArtifactId) -> Result<&ArtifactContent, ArtifactError> {
        self.read_record(id).map(ArtifactRecord::content)
    }

    /// Creates an evidence reference only if the target artifact and locator are readable.
    pub fn evidence_ref(
        &self,
        artifact_id: &ArtifactId,
        locator: EvidenceLocator,
    ) -> Result<EvidenceRef, ArtifactError> {
        let record = self.read_record(artifact_id)?;
        validate_locator(record.artifact.id(), record.content(), &locator)?;
        Ok(EvidenceRef::new(artifact_id.clone(), locator))
    }

    /// Validates that a recorded evidence reference can retrieve exact content.
    pub fn validate_evidence(&self, evidence: &EvidenceRef) -> Result<(), ArtifactError> {
        let record = self.read_record(&evidence.artifact_id)?;
        validate_locator(record.artifact.id(), record.content(), &evidence.locator)
    }

    /// Reads exact evidence content referenced by a recorded evidence reference.
    pub fn read_evidence(&self, evidence: &EvidenceRef) -> Result<ArtifactContent, ArtifactError> {
        let record = self.read_record(&evidence.artifact_id)?;
        read_located_content(record.artifact.id(), record.content(), &evidence.locator)
    }
}

fn validate_content_kind(
    artifact: &ArtifactRef,
    content: &ArtifactContent,
) -> Result<(), ArtifactError> {
    let expected = content_kind_for_artifact(artifact.kind());
    let actual = content.kind();
    if expected == actual {
        return Ok(());
    }

    Err(ArtifactError::IncompatibleContent {
        id: artifact.id().clone(),
        artifact_kind: artifact.kind().clone(),
        content_kind: actual,
    })
}

fn content_kind_for_artifact(kind: &ArtifactKind) -> ArtifactContentKind {
    match kind {
        ArtifactKind::Text => ArtifactContentKind::Text,
        ArtifactKind::Json => ArtifactContentKind::Json,
        ArtifactKind::Binary => ArtifactContentKind::Binary,
        ArtifactKind::Image => ArtifactContentKind::Image,
        ArtifactKind::Other => ArtifactContentKind::Other,
    }
}

fn validate_locator(
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
        validate_byte_range(artifact_id, content, start, end)
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

fn validate_byte_range(
    artifact_id: &ArtifactId,
    content: &ArtifactContent,
    start: u64,
    end: u64,
) -> Result<(), ArtifactError> {
    let bytes = content.as_bytes();
    let Ok(start) = usize::try_from(start) else {
        return Err(invalid_locator(
            artifact_id,
            "byte range start is outside platform bounds",
        ));
    };
    let Ok(end) = usize::try_from(end) else {
        return Err(invalid_locator(
            artifact_id,
            "byte range end is outside platform bounds",
        ));
    };

    if end > bytes.len() {
        return Err(invalid_locator(
            artifact_id,
            "byte range is outside artifact content",
        ));
    }

    match content {
        ArtifactContent::Text(text) | ArtifactContent::Json(text) => {
            text.get(start..end).map(|_| ()).ok_or_else(|| {
                invalid_locator(
                    artifact_id,
                    "byte range must align to utf-8 character boundaries for textual content",
                )
            })
        }
        ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
            Ok(())
        }
    }
}

fn read_located_content(
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
        Ok(ArtifactContent::Text(text[start..end].to_owned()))
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

fn read_byte_range(
    artifact_id: &ArtifactId,
    content: &ArtifactContent,
    start: u64,
    end: u64,
) -> Result<ArtifactContent, ArtifactError> {
    validate_byte_range(artifact_id, content, start, end)?;
    let start = usize::try_from(start).expect("validated byte range start fits usize");
    let end = usize::try_from(end).expect("validated byte range end fits usize");

    match content {
        ArtifactContent::Text(text) | ArtifactContent::Json(text) => {
            Ok(ArtifactContent::Text(text[start..end].to_owned()))
        }
        ArtifactContent::Binary(bytes) => Ok(ArtifactContent::Binary(bytes[start..end].to_vec())),
        ArtifactContent::Image(bytes) => Ok(ArtifactContent::Image(bytes[start..end].to_vec())),
        ArtifactContent::Other(bytes) => Ok(ArtifactContent::Other(bytes[start..end].to_vec())),
    }
}

fn line_range_bounds(text: &str, start: u64, end: u64) -> Option<(usize, usize)> {
    debug_assert!(start <= end);

    let mut line_number = 1_u64;
    let mut line_start = 0_usize;
    let mut selected_start = None;

    for segment in text.split_inclusive('\n') {
        let line_end = line_start + segment.len();
        if line_number == start {
            selected_start = Some(line_start);
        }

        if line_number == end {
            let content_end = line_content_end(text.as_bytes(), line_start, line_end);
            return selected_start.map(|start| (start, content_end));
        }

        line_start = line_end;
        line_number += 1;
    }

    None
}

fn line_content_end(bytes: &[u8], line_start: usize, line_end: usize) -> usize {
    let mut content_end = line_end;
    if content_end > line_start && bytes[content_end - 1] == b'\n' {
        content_end -= 1;
        if content_end > line_start && bytes[content_end - 1] == b'\r' {
            content_end -= 1;
        }
    }
    content_end
}

fn invalid_locator(artifact_id: &ArtifactId, reason: &'static str) -> ArtifactError {
    ArtifactError::InvalidEvidenceLocator {
        id: artifact_id.clone(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::{ArtifactContent, ArtifactRegistry};
    use merry_core::{ArtifactId, ArtifactKind, ArtifactRef};
    use std::sync::Arc;

    fn artifact_id(value: &str) -> ArtifactId {
        ArtifactId::new(value).expect("valid artifact id")
    }

    fn artifact_ref(value: &str, kind: ArtifactKind) -> ArtifactRef {
        ArtifactRef::new(artifact_id(value), kind)
    }

    #[test]
    fn cloned_registry_shares_recorded_content_storage() {
        let mut registry = ArtifactRegistry::default();
        let artifact = artifact_ref("large-tool-output", ArtifactKind::Text);
        registry
            .record(
                artifact.clone(),
                ArtifactContent::text("large exact output\n".repeat(1024)),
            )
            .expect("artifact should record");

        let cloned = registry.clone();

        assert_eq!(
            registry
                .read_content(artifact.id())
                .expect("original content should be readable"),
            cloned
                .read_content(artifact.id())
                .expect("cloned content should be readable")
        );

        let original_record = registry
            .read_record(artifact.id())
            .expect("original record should be readable");
        let cloned_record = cloned
            .read_record(artifact.id())
            .expect("cloned record should be readable");

        assert!(Arc::ptr_eq(
            &original_record.content,
            &cloned_record.content
        ));
    }
}
