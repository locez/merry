//! Evidence reference vocabulary.

use crate::{ArtifactId, CoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

const MAX_SECTION_NAME_LEN: usize = 128;

/// A location inside an artifact that supports exact evidence retrieval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct EvidenceLocator(EvidenceLocatorRepr);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum EvidenceLocatorRepr {
    /// The entire artifact is evidence.
    WholeArtifact,
    /// Inclusive 1-based line range.
    LineRange { start: u64, end: u64 },
    /// Half-open byte range.
    ByteRange { start: u64, end: u64 },
    /// Non-empty RFC 6901 JSON pointer.
    ///
    /// Empty JSON Pointer denotes the whole document and is represented by
    /// [`EvidenceLocator::whole_artifact`].
    JsonPointer { pointer: String },
    /// Named artifact section.
    NamedSection { name: String },
}

impl EvidenceLocator {
    /// References the whole artifact.
    #[must_use]
    pub fn whole_artifact() -> Self {
        Self(EvidenceLocatorRepr::WholeArtifact)
    }

    /// Creates an inclusive 1-based line range.
    pub fn line_range(start: u64, end: u64) -> Result<Self, CoreError> {
        if start == 0 {
            return Err(invalid_locator(
                "line range",
                format!("{start}..={end}"),
                "line range start must be greater than zero",
            ));
        }

        if start > end {
            return Err(invalid_locator(
                "line range",
                format!("{start}..={end}"),
                "line range start must be less than or equal to end",
            ));
        }

        Ok(Self(EvidenceLocatorRepr::LineRange { start, end }))
    }

    /// Creates a half-open byte range.
    pub fn byte_range(start: u64, end: u64) -> Result<Self, CoreError> {
        if start >= end {
            return Err(invalid_locator(
                "byte range",
                format!("{start}..{end}"),
                "byte range start must be less than end",
            ));
        }

        Ok(Self(EvidenceLocatorRepr::ByteRange { start, end }))
    }

    /// Creates a non-empty JSON pointer locator.
    ///
    /// Empty JSON Pointer is deliberately rejected because
    /// [`EvidenceLocator::whole_artifact`] represents the whole artifact.
    pub fn json_pointer(pointer: &str) -> Result<Self, CoreError> {
        validate_json_pointer(pointer)?;
        Ok(Self(EvidenceLocatorRepr::JsonPointer {
            pointer: pointer.to_owned(),
        }))
    }

    /// Creates a named section locator.
    pub fn named_section(name: &str) -> Result<Self, CoreError> {
        validate_section_name(name)?;
        Ok(Self(EvidenceLocatorRepr::NamedSection {
            name: name.to_owned(),
        }))
    }

    /// Returns whether this locator references the whole artifact.
    #[must_use]
    pub fn is_whole_artifact(&self) -> bool {
        matches!(self.0, EvidenceLocatorRepr::WholeArtifact)
    }

    /// Returns the inclusive 1-based line range when this locator is a line range.
    #[must_use]
    pub fn as_line_range(&self) -> Option<(u64, u64)> {
        match self.0 {
            EvidenceLocatorRepr::LineRange { start, end } => Some((start, end)),
            _ => None,
        }
    }

    /// Returns the half-open byte range when this locator is a byte range.
    #[must_use]
    pub fn as_byte_range(&self) -> Option<(u64, u64)> {
        match self.0 {
            EvidenceLocatorRepr::ByteRange { start, end } => Some((start, end)),
            _ => None,
        }
    }

    /// Returns the JSON pointer when this locator is a JSON pointer.
    #[must_use]
    pub fn as_json_pointer(&self) -> Option<&str> {
        match &self.0 {
            EvidenceLocatorRepr::JsonPointer { pointer } => Some(pointer),
            _ => None,
        }
    }

    /// Returns the section name when this locator is a named section.
    #[must_use]
    pub fn as_named_section(&self) -> Option<&str> {
        match &self.0 {
            EvidenceLocatorRepr::NamedSection { name } => Some(name),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum EvidenceLocatorWire {
    WholeArtifact,
    LineRange { start: u64, end: u64 },
    ByteRange { start: u64, end: u64 },
    JsonPointer { pointer: String },
    NamedSection { name: String },
}

impl<'de> Deserialize<'de> for EvidenceLocator {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = EvidenceLocatorWire::deserialize(deserializer)?;
        match wire {
            EvidenceLocatorWire::WholeArtifact => Ok(Self::whole_artifact()),
            EvidenceLocatorWire::LineRange { start, end } => {
                Self::line_range(start, end).map_err(de::Error::custom)
            }
            EvidenceLocatorWire::ByteRange { start, end } => {
                Self::byte_range(start, end).map_err(de::Error::custom)
            }
            EvidenceLocatorWire::JsonPointer { pointer } => {
                Self::json_pointer(&pointer).map_err(de::Error::custom)
            }
            EvidenceLocatorWire::NamedSection { name } => {
                Self::named_section(&name).map_err(de::Error::custom)
            }
        }
    }
}

/// Reference to exact evidence in an artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    /// Artifact containing the evidence.
    pub artifact_id: ArtifactId,
    /// Exact location inside the artifact.
    pub locator: EvidenceLocator,
}

impl EvidenceRef {
    /// Creates an evidence reference.
    #[must_use]
    pub fn new(artifact_id: ArtifactId, locator: EvidenceLocator) -> Self {
        Self {
            artifact_id,
            locator,
        }
    }
}

fn validate_json_pointer(pointer: &str) -> Result<(), CoreError> {
    if pointer.is_empty() {
        return Err(invalid_locator(
            "JSON pointer",
            pointer,
            "empty JSON Pointer denotes the whole artifact; use WholeArtifact",
        ));
    }

    if !pointer.starts_with('/') {
        return Err(invalid_locator(
            "JSON pointer",
            pointer,
            "JSON pointer must start with '/'",
        ));
    }

    if pointer.chars().any(char::is_control) {
        return Err(invalid_locator(
            "JSON pointer",
            pointer,
            "JSON pointer must not contain control characters",
        ));
    }

    let mut chars = pointer.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0' | '1') => {}
                _ => {
                    return Err(invalid_locator(
                        "JSON pointer",
                        pointer,
                        "JSON pointer '~' escapes must be '~0' or '~1'",
                    ));
                }
            }
        }
    }

    Ok(())
}

fn validate_section_name(name: &str) -> Result<(), CoreError> {
    if name.trim().is_empty() {
        return Err(invalid_locator(
            "named section",
            name,
            "named section must not be blank",
        ));
    }

    if name.trim() != name {
        return Err(invalid_locator(
            "named section",
            name,
            "named section must not have leading or trailing whitespace",
        ));
    }

    if name.chars().count() > MAX_SECTION_NAME_LEN {
        return Err(invalid_locator(
            "named section",
            name,
            "named section is longer than the allowed maximum length",
        ));
    }

    if name.chars().any(char::is_control) {
        return Err(invalid_locator(
            "named section",
            name,
            "named section must not contain control characters",
        ));
    }

    Ok(())
}

fn invalid_locator(
    kind: &'static str,
    value: impl Into<String>,
    reason: &'static str,
) -> CoreError {
    CoreError::InvalidEvidenceLocator {
        kind,
        value: value.into(),
        reason,
    }
}
