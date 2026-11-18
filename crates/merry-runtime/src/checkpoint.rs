use thiserror::Error;

mod reference;

pub use reference::{CheckpointRef, CheckpointRefManifest, CheckpointSourceKind};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CheckpointError {
    #[error("{field} must not be blank")]
    BlankField { field: &'static str },

    #[error("{field} must be a provider-safe identifier")]
    InvalidIdentifier { field: &'static str },

    #[error("checkpoint sequence range start {start} must be <= end {end}")]
    InvalidSequenceRange { start: u64, end: u64 },

    #[error("checkpoint ref {ref_id} is duplicated")]
    DuplicateRef { ref_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckpointId(String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckpointRefId(String);

macro_rules! impl_checkpoint_id {
    ($type:ident, $field:literal) => {
        impl $type {
            pub fn new(value: &str) -> Result<Self, CheckpointError> {
                validate_identifier($field, value)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

impl_checkpoint_id!(CheckpointId, "checkpoint id");
impl_checkpoint_id!(CheckpointRefId, "checkpoint ref id");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointSequenceRange {
    start: u64,
    end: u64,
}

impl CheckpointSequenceRange {
    pub fn new(start: u64, end: u64) -> Result<Self, CheckpointError> {
        if start > end {
            return Err(CheckpointError::InvalidSequenceRange { start, end });
        }
        Ok(Self { start, end })
    }

    #[must_use]
    pub fn start(&self) -> u64 {
        self.start
    }

    #[must_use]
    pub fn end(&self) -> u64 {
        self.end
    }
}

pub(super) fn validate_identifier(field: &'static str, value: &str) -> Result<(), CheckpointError> {
    if value.trim().is_empty() {
        return Err(CheckpointError::BlankField { field });
    }
    if value.trim() != value {
        return Err(CheckpointError::InvalidIdentifier { field });
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
    {
        return Ok(());
    }
    Err(CheckpointError::InvalidIdentifier { field })
}
