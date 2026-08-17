use schemars::{Schema, SchemaGenerator, json_schema};
use thiserror::Error;

pub(crate) const RELATIVE_PATH_PATTERN: &str = r"^(?:[^./\\\x00-\x1f\x7f-\x9f](?:[^/\\:\x00-\x1f\x7f-\x9f][^/\\\x00-\x1f\x7f-\x9f]*)?|\.[^./\\:\x00-\x1f\x7f-\x9f][^/\\\x00-\x1f\x7f-\x9f]*|\.\.[^/\\\x00-\x1f\x7f-\x9f]+)(?:/(?:[^./\\\x00-\x1f\x7f-\x9f](?:[^/\\:\x00-\x1f\x7f-\x9f][^/\\\x00-\x1f\x7f-\x9f]*)?|\.[^./\\:\x00-\x1f\x7f-\x9f][^/\\\x00-\x1f\x7f-\x9f]*|\.\.[^/\\\x00-\x1f\x7f-\x9f]+))*$";
pub(crate) const IDENTIFIER_PATTERN: &str =
    r"^[^\s\x00-\x1f\x7f-\x9f](?:[^\x00-\x1f\x7f-\x9f]*[^\s\x00-\x1f\x7f-\x9f])?$";
pub(crate) const NON_BLANK_TEXT_PATTERN: &str = r"^[^\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]*[^\s\x00-\x1f\x7f-\x9f][^\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]*$";
const SHA256_PATTERN: &str = r"^[0-9A-Fa-f]{64}$";
const MAX_RELATIVE_PATH_CHARS: usize = 512;

pub(crate) fn relative_path_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "string",
        "minLength": 1,
        "maxLength": MAX_RELATIVE_PATH_CHARS,
        "pattern": RELATIVE_PATH_PATTERN,
    })
}

pub(crate) fn optional_relative_path_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": ["string", "null"],
        "minLength": 1,
        "maxLength": MAX_RELATIVE_PATH_CHARS,
        "pattern": RELATIVE_PATH_PATTERN,
    })
}

pub(crate) fn optional_sha256_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": ["string", "null"],
        "pattern": SHA256_PATTERN,
    })
}

/// Errors raised while defining or serializing evaluation protocol data.
#[derive(Debug, Error)]
pub enum EvalError {
    /// The task manifest was not valid TOML.
    #[error("failed to parse task manifest: {0}")]
    ManifestParse(#[from] toml::de::Error),

    /// A task manifest could not be serialized as TOML.
    #[error("failed to serialize task manifest: {0}")]
    ManifestSerialize(#[from] toml::ser::Error),

    /// A JSONL evaluation record could not be encoded or decoded.
    #[error("failed to encode or decode evaluation record: {0}")]
    RecordJson(#[from] serde_json::Error),

    /// A manifest or record used a protocol version this crate does not know.
    #[error("unsupported {kind} version {found}; supported version is {supported}")]
    UnsupportedVersion {
        /// The protocol object carrying the version.
        kind: &'static str,
        /// The version found in the input.
        found: u32,
        /// The newest version understood by this crate.
        supported: u32,
    },

    /// An external protocol value failed a domain validation rule.
    #[error("invalid {field}: {reason}")]
    InvalidField {
        /// The field or nested path that failed validation.
        field: String,
        /// The actionable reason for rejection.
        reason: String,
    },

    /// A JSONL record did not contain exactly one framed JSON line.
    #[error("invalid evaluation record framing: {0}")]
    InvalidRecordFraming(String),
}

pub(crate) fn invalid_field(field: impl Into<String>, reason: impl Into<String>) -> EvalError {
    EvalError::InvalidField {
        field: field.into(),
        reason: reason.into(),
    }
}

pub(crate) fn validate_identifier(
    field: &str,
    value: &str,
    max_chars: usize,
) -> Result<(), EvalError> {
    if value.is_empty() {
        return Err(invalid_field(field, "must not be empty"));
    }
    if value.trim() != value {
        return Err(invalid_field(
            field,
            "must not have leading or trailing whitespace",
        ));
    }
    if value.chars().count() > max_chars {
        return Err(invalid_field(
            field,
            format!("must contain at most {max_chars} characters"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid_field(field, "must not contain control characters"));
    }
    Ok(())
}

pub(crate) fn validate_text(field: &str, value: &str, max_chars: usize) -> Result<(), EvalError> {
    if value.trim().is_empty() {
        return Err(invalid_field(field, "must not be blank"));
    }
    if value.chars().count() > max_chars {
        return Err(invalid_field(
            field,
            format!("must contain at most {max_chars} characters"),
        ));
    }
    if value.chars().any(|character| {
        character == '\0' || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    }) {
        return Err(invalid_field(
            field,
            "must not contain unsafe control characters",
        ));
    }
    Ok(())
}

pub(crate) fn validate_relative_path(field: &str, value: &str) -> Result<(), EvalError> {
    if value.is_empty() {
        return Err(invalid_field(field, "must not be empty"));
    }
    if value.starts_with('/') || value.starts_with('\\') || value.contains('\\') {
        return Err(invalid_field(
            field,
            "must be a relative slash-separated path",
        ));
    }
    if value.chars().count() > MAX_RELATIVE_PATH_CHARS {
        return Err(invalid_field(
            field,
            format!("must contain at most {MAX_RELATIVE_PATH_CHARS} characters"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid_field(field, "must not contain control characters"));
    }
    if value.split('/').any(|component| {
        component.is_empty()
            || component == "."
            || component == ".."
            || component.chars().nth(1) == Some(':')
    }) {
        return Err(invalid_field(field, "must not escape the task workspace"));
    }
    Ok(())
}
