//! Artifact reference vocabulary.

use crate::{ArtifactId, CoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

const MAX_ARTIFACT_LABEL_LEN: usize = 128;

/// Provider-neutral artifact kind.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// UTF-8 text.
    Text,
    /// Structured JSON.
    Json,
    /// Opaque binary data.
    Binary,
    /// Image data.
    Image,
    /// A provider-neutral kind not covered by the stable variants.
    Other,
}

/// Reference to an artifact recorded elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// Stable artifact identifier.
    id: ArtifactId,
    /// Provider-neutral artifact kind.
    kind: ArtifactKind,
    /// Optional human-readable label.
    #[schemars(extend("x-merry-output-required" = true))]
    label: Option<String>,
}

impl ArtifactRef {
    /// Creates an artifact reference without a label.
    #[must_use]
    pub fn new(id: ArtifactId, kind: ArtifactKind) -> Self {
        Self {
            id,
            kind,
            label: None,
        }
    }

    /// Borrows the artifact identifier.
    #[must_use]
    pub fn id(&self) -> &ArtifactId {
        &self.id
    }

    /// Borrows the artifact kind.
    #[must_use]
    pub fn kind(&self) -> &ArtifactKind {
        &self.kind
    }

    /// Borrows the optional human-readable label.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Adds a validated human-readable label to the artifact reference.
    pub fn with_label(mut self, label: &str) -> Result<Self, CoreError> {
        validate_label(label)?;
        self.label = Some(label.to_owned());
        Ok(self)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRefWire {
    id: ArtifactId,
    kind: ArtifactKind,
    label: Option<String>,
}

impl<'de> Deserialize<'de> for ArtifactRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ArtifactRefWire::deserialize(deserializer)?;
        if let Some(label) = wire.label.as_deref() {
            validate_label(label).map_err(de::Error::custom)?;
        }

        Ok(Self {
            id: wire.id,
            kind: wire.kind,
            label: wire.label,
        })
    }
}

fn validate_label(label: &str) -> Result<(), CoreError> {
    if label.trim().is_empty() {
        return Err(CoreError::InvalidIdentifier {
            kind: "ArtifactRef label",
            value: label.to_owned(),
            reason: "must not be blank",
        });
    }

    if label.trim() != label {
        return Err(CoreError::InvalidIdentifier {
            kind: "ArtifactRef label",
            value: label.to_owned(),
            reason: "must not have leading or trailing whitespace",
        });
    }

    if label.chars().count() > MAX_ARTIFACT_LABEL_LEN {
        return Err(CoreError::InvalidIdentifier {
            kind: "ArtifactRef label",
            value: label.to_owned(),
            reason: "is longer than the allowed maximum length",
        });
    }

    if label.chars().any(char::is_control) {
        return Err(CoreError::InvalidIdentifier {
            kind: "ArtifactRef label",
            value: label.to_owned(),
            reason: "must not contain control characters",
        });
    }

    Ok(())
}
