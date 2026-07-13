//! In-memory artifact registry.
//!
//! [`ArtifactContent`] and [`ArtifactError`] are the MVP runtime boundary for
//! exact artifact payloads and artifact-state failures. [`ArtifactRegistry`] is
//! a low-level in-memory implementation aid for session state and tests.
//!
//! External callers should prefer [`crate::Runtime::record_artifact`] and
//! [`crate::Runtime::evidence_ref`] when working with session-owned state. That
//! facade enforces runtime artifact-id ownership and records lifecycle facts
//! before observable events.

use merry_core::{ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use thiserror::Error;

/// Exact content stored for an artifact.
///
/// This enum is the MVP payload boundary for runtime-owned artifacts. Variants
/// are provider-neutral and intentionally mirror Merry artifact kinds rather
/// than provider wire content blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactContent {
    /// UTF-8 text.
    Text { content: String },
    /// Serialized JSON text.
    Json { content: String },
    /// Opaque binary bytes.
    Binary { bytes: Vec<u8> },
    /// Image bytes with optional normalized image metadata.
    Image {
        bytes: Arc<[u8]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Box<ImageArtifactMetadata>>,
    },
    /// Provider-neutral bytes for artifact kinds not covered by stable variants.
    Other { bytes: Vec<u8> },
}

impl ArtifactContent {
    /// Creates UTF-8 text artifact content.
    pub fn text(content: impl Into<String>) -> Self {
        Self::Text {
            content: content.into(),
        }
    }

    /// Creates serialized JSON artifact content.
    ///
    /// The runtime stores JSON as exact text in the MVP; it does not parse or
    /// rewrite the payload.
    pub fn json(content: impl Into<String>) -> Self {
        Self::Json {
            content: content.into(),
        }
    }

    /// Creates opaque binary artifact content.
    pub fn binary(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Binary {
            bytes: bytes.into(),
        }
    }

    /// Creates image artifact content.
    pub fn image(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self::Image {
            bytes: bytes.into(),
            metadata: None,
        }
    }

    pub(crate) fn normalized_png(bytes: impl Into<Arc<[u8]>>, width: u32, height: u32) -> Self {
        Self::Image {
            bytes: bytes.into(),
            metadata: Some(Box::new(ImageArtifactMetadata {
                media_type: "image/png".to_owned(),
                width,
                height,
            })),
        }
    }

    /// Creates content for provider-neutral artifact kinds not covered by stable variants.
    pub fn other(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Other {
            bytes: bytes.into(),
        }
    }

    /// Returns the content kind.
    #[must_use]
    pub fn kind(&self) -> ArtifactContentKind {
        match self {
            Self::Text { .. } => ArtifactContentKind::Text,
            Self::Json { .. } => ArtifactContentKind::Json,
            Self::Binary { .. } => ArtifactContentKind::Binary,
            Self::Image { .. } => ArtifactContentKind::Image,
            Self::Other { .. } => ArtifactContentKind::Other,
        }
    }

    /// Borrows textual artifact content.
    ///
    /// JSON content is returned as text because the MVP registry preserves the
    /// exact serialized payload.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { content } | Self::Json { content } => Some(content),
            Self::Binary { .. } | Self::Image { .. } | Self::Other { .. } => None,
        }
    }

    /// Borrows artifact content as exact bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Text { content } | Self::Json { content } => content.as_bytes(),
            Self::Binary { bytes } | Self::Other { bytes } => bytes,
            Self::Image { bytes, .. } => bytes,
        }
    }
}

/// Optional metadata for image artifacts with a known decoded representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageArtifactMetadata {
    media_type: String,
    width: u32,
    height: u32,
}

impl ImageArtifactMetadata {
    /// Declared media type of the exact image bytes.
    #[must_use]
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    /// Decoded image width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Decoded image height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }
}

/// Artifact content category used for metadata/content compatibility checks.
///
/// This is a runtime-local category, not a provider media type registry.
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
///
/// Records are exposed for the low-level in-memory registry. Session callers
/// should usually keep using [`crate::Runtime`] methods so artifact ownership
/// and event ordering remain centralized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    artifact: ArtifactRef,
    content: Arc<ArtifactContent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedArtifactRecord {
    pub(crate) artifact: ArtifactRef,
    pub(crate) content: ArtifactContent,
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

/// One bounded UTF-8 page read from an exact evidence reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEvidencePage {
    artifact_id: ArtifactId,
    content: String,
    offset: usize,
    next_offset: Option<usize>,
    total_bytes: usize,
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

/// Errors raised by artifact registry operations.
///
/// These errors describe artifact-state validation and read failures at the MVP
/// boundary. Runtime facade methods wrap them in [`crate::RuntimeError`].
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

    /// The requested evidence page cannot be represented safely.
    #[error("artifact id {id} has invalid evidence page: {reason}")]
    InvalidEvidencePage {
        /// Artifact identifier.
        id: ArtifactId,
        /// Actionable reason.
        reason: &'static str,
    },

    /// The requested evidence is not UTF-8 text.
    #[error("artifact id {id} is not textual evidence")]
    NonTextEvidencePage {
        /// Artifact identifier.
        id: ArtifactId,
    },
}

/// In-memory artifact reference and content registry.
///
/// Recording returns an [`ArtifactRef`] only after metadata and exact content
/// have been stored, keeping state-before-reference usage natural for callers.
///
/// This registry is a low-level implementation aid for the current in-memory
/// runtime. It does not enforce session-level policies such as reserved runtime
/// artifact ids; use [`crate::Runtime::record_artifact`] for session-owned
/// external recording.
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
    ///
    /// The registry validates metadata/content compatibility but does not emit
    /// runtime events or lifecycle facts.
    pub fn record(
        &mut self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<ArtifactRef, ArtifactError> {
        self.ensure_recordable(&artifact, &content)?;
        Ok(self.record_preflighted(artifact, content))
    }

    pub(crate) fn ensure_recordable(
        &self,
        artifact: &ArtifactRef,
        content: &ArtifactContent,
    ) -> Result<(), ArtifactError> {
        if self.records.contains_key(artifact.id()) {
            return Err(ArtifactError::DuplicateId {
                id: artifact.id().clone(),
            });
        }

        validate_content_kind(artifact, content)?;
        Ok(())
    }

    pub(crate) fn record_preflighted(
        &mut self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> ArtifactRef {
        debug_assert!(self.ensure_recordable(&artifact, &content).is_ok());
        let id = artifact.id().clone();
        let recorded = artifact.clone();
        let previous = self.records.insert(
            id,
            ArtifactRecord {
                artifact,
                content: Arc::new(content),
            },
        );
        debug_assert!(previous.is_none());
        recorded
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
    ///
    /// Prefer [`crate::Runtime::evidence_ref`] for session-owned state.
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

    /// Validates that an evidence reference can be read through text paging.
    ///
    /// This performs no content allocation. Text and JSON artifacts are
    /// accepted when the locator selects a valid range; binary and media
    /// artifacts are rejected even when their generic evidence locator is valid.
    pub fn validate_text_evidence(&self, evidence: &EvidenceRef) -> Result<(), ArtifactError> {
        self.validated_text_evidence_range(evidence).map(|_| ())
    }

    /// Reads exact evidence content referenced by a recorded evidence reference.
    ///
    /// The returned content is a cloned exact slice or payload for the selected
    /// locator.
    pub fn read_evidence(&self, evidence: &EvidenceRef) -> Result<ArtifactContent, ArtifactError> {
        let record = self.read_record(&evidence.artifact_id)?;
        read_located_content(record.artifact.id(), record.content(), &evidence.locator)
    }

    /// Reads one bounded UTF-8 page from the selected evidence range.
    ///
    /// `offset` is measured from the start of the selected evidence range, not
    /// from the start of the containing artifact.
    pub fn read_text_evidence_page(
        &self,
        evidence: &EvidenceRef,
        offset: usize,
        max_bytes: usize,
    ) -> Result<TextEvidencePage, ArtifactError> {
        let (text, range_start, range_end) = self.validated_text_evidence_range(evidence)?;
        let total_bytes = range_end - range_start;

        if max_bytes == 0 {
            return Err(invalid_page(
                &evidence.artifact_id,
                "max_bytes must be greater than zero",
            ));
        }
        if offset > total_bytes {
            return Err(invalid_page(
                &evidence.artifact_id,
                "offset is outside the selected evidence range",
            ));
        }

        let page_start = range_start + offset;
        if !text.is_char_boundary(page_start) {
            return Err(invalid_page(
                &evidence.artifact_id,
                "offset must align to a UTF-8 character boundary",
            ));
        }
        if offset == total_bytes {
            return Ok(TextEvidencePage {
                artifact_id: evidence.artifact_id.clone(),
                content: String::new(),
                offset,
                next_offset: None,
                total_bytes,
            });
        }

        let requested_end = offset.saturating_add(max_bytes).min(total_bytes);
        let mut page_end = range_start + requested_end;
        while page_end > page_start && !text.is_char_boundary(page_end) {
            page_end -= 1;
        }
        if page_end == page_start {
            return Err(invalid_page(
                &evidence.artifact_id,
                "max_bytes is too small to include the next UTF-8 character",
            ));
        }

        let consumed_end = page_end - range_start;
        Ok(TextEvidencePage {
            artifact_id: evidence.artifact_id.clone(),
            content: text[page_start..page_end].to_owned(),
            offset,
            next_offset: (consumed_end < total_bytes).then_some(consumed_end),
            total_bytes,
        })
    }

    fn validated_text_evidence_range<'a>(
        &'a self,
        evidence: &EvidenceRef,
    ) -> Result<(&'a str, usize, usize), ArtifactError> {
        let record = self.read_record(&evidence.artifact_id)?;
        let Some(text) = record.content().as_text() else {
            return Err(ArtifactError::NonTextEvidencePage {
                id: evidence.artifact_id.clone(),
            });
        };
        let (range_start, range_end) =
            text_evidence_bounds(record.artifact.id(), record.content(), &evidence.locator)?;
        Ok((text, range_start, range_end))
    }

    pub(crate) fn persisted_records(&self) -> Vec<PersistedArtifactRecord> {
        self.records
            .values()
            .map(|record| PersistedArtifactRecord {
                artifact: record.artifact().clone(),
                content: record.content().clone(),
            })
            .collect()
    }

    pub(crate) fn from_persisted_records(
        records: Vec<PersistedArtifactRecord>,
    ) -> Result<Self, ArtifactError> {
        let mut registry = Self::default();
        for record in records {
            registry.record(record.artifact, record.content)?;
        }
        Ok(registry)
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

fn text_evidence_bounds(
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
        validate_byte_range(artifact_id, content, start, end)?;
        Ok((
            usize::try_from(start).expect("validated byte range start fits usize"),
            usize::try_from(end).expect("validated byte range end fits usize"),
        ))
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
        ArtifactContent::Text { content: text } | ArtifactContent::Json { content: text } => {
            text.get(start..end).map(|_| ()).ok_or_else(|| {
                invalid_locator(
                    artifact_id,
                    "byte range must align to utf-8 character boundaries for textual content",
                )
            })
        }
        ArtifactContent::Binary { .. }
        | ArtifactContent::Image { .. }
        | ArtifactContent::Other { .. } => Ok(()),
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

fn line_range_bounds(text: &str, start: u64, end: u64) -> Option<(usize, usize)> {
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

fn invalid_page(artifact_id: &ArtifactId, reason: &'static str) -> ArtifactError {
    ArtifactError::InvalidEvidencePage {
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
