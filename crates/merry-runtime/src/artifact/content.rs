//! Exact artifact payloads, bounded previews, and content-kind validation.

use crate::artifact::ArtifactError;
use merry_core::{ArtifactKind, ArtifactRef};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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

    /// Creates a bounded text preview without cloning the full artifact.
    #[must_use]
    pub fn bounded_text(&self, max_bytes: usize) -> (Option<String>, bool) {
        let Some(value) = self.as_text() else {
            return (None, false);
        };
        if value.len() <= max_bytes {
            return (Some(value.to_owned()), false);
        }
        let mut end = 0;
        for (index, character) in value.char_indices() {
            let next = index + character.len_utf8();
            if next > max_bytes {
                break;
            }
            end = next;
        }
        (Some(value[..end].to_owned()), true)
    }
}

/// Optional metadata for image artifacts with a known decoded representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageArtifactMetadata {
    pub(super) media_type: String,
    pub(super) width: u32,
    pub(super) height: u32,
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

/// Bounded inspection data for an artifact without exposing its full payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactContentPreview {
    pub(super) kind: ArtifactContentKind,
    pub(super) content: Option<String>,
    pub(super) truncated: bool,
    pub(super) byte_length: usize,
}

impl ArtifactContentPreview {
    /// Returns the artifact content category.
    #[must_use]
    pub fn kind(&self) -> ArtifactContentKind {
        self.kind
    }

    /// Returns the bounded text payload, when the artifact is textual.
    #[must_use]
    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    /// Returns whether the exact textual payload exceeded the preview bound.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Returns the exact artifact byte length.
    #[must_use]
    pub fn byte_length(&self) -> usize {
        self.byte_length
    }
}

pub(super) fn validate_content_kind(
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

pub(super) fn content_kind_for_artifact(kind: &ArtifactKind) -> ArtifactContentKind {
    match kind {
        ArtifactKind::Text => ArtifactContentKind::Text,
        ArtifactKind::Json => ArtifactContentKind::Json,
        ArtifactKind::Binary => ArtifactContentKind::Binary,
        ArtifactKind::Image => ArtifactContentKind::Image,
        ArtifactKind::Other => ArtifactContentKind::Other,
    }
}
