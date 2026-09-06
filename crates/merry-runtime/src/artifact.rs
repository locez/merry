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

use crate::artifact::{
    content::validate_content_kind,
    evidence::{invalid_page, read_located_content, text_evidence_bounds, validate_locator},
};
pub use content::{
    ArtifactContent, ArtifactContentKind, ArtifactContentPreview, ImageArtifactMetadata,
};
pub use evidence::TextEvidencePage;
use merry_core::{ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use thiserror::Error;

mod content;

mod evidence;

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

    /// Reads bounded inspection data without cloning the full artifact payload.
    pub fn read_content_preview(
        &self,
        id: &ArtifactId,
        max_bytes: usize,
    ) -> Result<ArtifactContentPreview, ArtifactError> {
        let content = self.read_content(id)?;
        let (preview, truncated) = content.bounded_text(max_bytes);
        Ok(ArtifactContentPreview {
            kind: content.kind(),
            content: preview,
            truncated,
            byte_length: content.as_bytes().len(),
        })
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

    #[test]
    fn bounded_preview_clones_only_a_utf8_safe_prefix() {
        let mut registry = ArtifactRegistry::default();
        let artifact = artifact_ref("preview", ArtifactKind::Text);
        registry
            .record(artifact.clone(), ArtifactContent::text("aébc"))
            .expect("artifact should record");

        let preview = registry
            .read_content_preview(artifact.id(), 3)
            .expect("preview should be readable");

        assert_eq!(preview.kind(), super::ArtifactContentKind::Text);
        assert_eq!(preview.content(), Some("aé"));
        assert!(preview.truncated());
        assert_eq!(preview.byte_length(), "aébc".len());
    }
}
