use thiserror::Error;

mod candidate;
mod domain;
mod reference;

#[cfg(test)]
mod tests;

pub use candidate::{
    CheckpointEntry, CheckpointEntryId, CheckpointHandoff, CheckpointHandoffAction,
    CheckpointSection, CheckpointSections, CompactedCheckpointCandidate,
};
pub(crate) use candidate::{
    PersistedCitationBackedCheckpoint, compacted_checkpoint_candidate_schema,
};
pub use domain::CitationBackedCheckpoint;
pub use reference::{CheckpointRef, CheckpointRefManifest, CheckpointSourceKind};

const DEFAULT_MAX_REF_EXCERPT_BYTES: usize = 1200;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CheckpointError {
    #[error("{field} must not be blank")]
    BlankField { field: &'static str },

    #[error("{field} must not contain control characters other than newline or tab")]
    InvalidControlCharacter { field: &'static str },

    #[error("{field} must be a provider-safe identifier")]
    InvalidIdentifier { field: &'static str },

    #[error("checkpoint sequence range start {start} must be <= end {end}")]
    InvalidSequenceRange { start: u64, end: u64 },

    #[error("checkpoint candidate JSON is invalid: {message}")]
    InvalidCandidateJson { message: String },

    #[error("checkpoint entry {entry_id} requires a non-blank rationale")]
    EntryRationaleRequired { entry_id: String },

    #[error("checkpoint entry {entry_id} has no refs")]
    EntryWithoutRefs { entry_id: String },

    #[error("checkpoint entry {entry_id} references unknown ref {ref_id}")]
    UnknownRef { entry_id: String, ref_id: String },

    #[error("checkpoint entry {entry_id} is duplicated across sections")]
    DuplicateEntry { entry_id: String },

    #[error("initial checkpoint must not contain handoffs")]
    InitialCheckpointHasHandoffs,

    #[error("checkpoint handoff for previous entry {old_id} is duplicated")]
    DuplicateHandoff { old_id: String },

    #[error("checkpoint pinned ref {ref_id} does not exist in the ref manifest")]
    PinnedRefNotFound { ref_id: String },

    #[error("manual checkpoint ref {ref_id} uses the runtime-reserved history ref namespace")]
    ManualCheckpointHistoryRefReserved { ref_id: String },

    #[error("checkpoint ref {ref_id} is duplicated")]
    DuplicateRef { ref_id: String },

    #[error("checkpoint ref {ref_id} does not exist in checkpoint {checkpoint_id}")]
    RefNotFound {
        checkpoint_id: String,
        ref_id: String,
    },

    #[error("checkpoint output is {actual_bytes} bytes, above accepted byte cap {max_bytes}")]
    OutputTooLarge {
        actual_bytes: usize,
        max_bytes: usize,
    },
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

impl CheckpointRefId {
    pub(crate) fn is_runtime_history_ref(&self) -> bool {
        self.as_str().strip_prefix('h').is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointValidationPolicy {
    max_ref_excerpt_bytes: usize,
}

impl CheckpointValidationPolicy {
    #[must_use]
    pub fn max_ref_excerpt_bytes(&self) -> usize {
        self.max_ref_excerpt_bytes
    }

    #[must_use]
    pub fn with_max_ref_excerpt_bytes(mut self, value: usize) -> Self {
        self.max_ref_excerpt_bytes = value;
        self
    }
}

impl Default for CheckpointValidationPolicy {
    fn default() -> Self {
        Self {
            max_ref_excerpt_bytes: DEFAULT_MAX_REF_EXCERPT_BYTES,
        }
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

pub(super) fn validate_text_field(field: &'static str, value: &str) -> Result<(), CheckpointError> {
    if value.trim().is_empty() {
        return Err(CheckpointError::BlankField { field });
    }
    if value
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(CheckpointError::InvalidControlCharacter { field });
    }
    Ok(())
}

pub(super) fn format_ref_list(refs: &[CheckpointRefId]) -> String {
    refs.iter()
        .map(CheckpointRefId::as_str)
        .collect::<Vec<_>>()
        .join(",")
}
