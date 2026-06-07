#![cfg_attr(not(test), allow(dead_code))]

use super::{audit::JudgmentRecordId, core::JudgmentPurpose};
use crate::artifact::ArtifactError;
use merry_core::ArtifactId;
use merry_llm::ProviderErrorKind;
use thiserror::Error;

/// Errors raised while building or requesting internal advisory judgments.
#[derive(Debug, Clone, PartialEq, Error)]
pub(crate) enum JudgmentError {
    /// A required text field was blank.
    #[error("{field} must not be blank")]
    BlankField {
        /// Name of the invalid field.
        field: &'static str,
    },

    /// Confidence was not finite or outside the valid range.
    #[error("judgment confidence {value} must be finite and inside the inclusive 0.0..=1.0 range")]
    InvalidConfidence {
        /// Rejected confidence value.
        value: f32,
    },

    /// A request had no semantic constraints.
    #[error("judgment request must include at least one runtime-owned constraint")]
    EmptyConstraints,

    /// Evidence was required for this judgment purpose or recommendation.
    #[error("{field} requires at least one exact evidence reference for {purpose}")]
    MissingEvidence {
        /// Judgment purpose that required evidence.
        purpose: JudgmentPurpose,
        /// Field that lacked evidence.
        field: &'static str,
    },

    /// Recommendation kind did not match the requested purpose.
    #[error("judgment recommendation {recommendation} is not valid for {purpose}")]
    RecommendationPurposeMismatch {
        /// Requested judgment purpose.
        purpose: JudgmentPurpose,
        /// Rejected recommendation kind.
        recommendation: &'static str,
    },

    /// Completed record request and outcome purposes did not match.
    #[error(
        "judgment outcome purpose {outcome_purpose} does not match request purpose {request_purpose}"
    )]
    RecordPurposeMismatch {
        /// Request purpose.
        request_purpose: JudgmentPurpose,
        /// Outcome purpose.
        outcome_purpose: JudgmentPurpose,
    },

    /// A narrow internal recording helper received an unsupported judgment purpose.
    #[error("{field} requires summary draft judgment purpose, got {actual_purpose}")]
    SummaryDraftPurposeRequired {
        /// Name of the rejected input field.
        field: &'static str,
        /// Rejected judgment purpose.
        actual_purpose: JudgmentPurpose,
    },

    /// The strict model judgment parser received an unsupported request purpose.
    #[error(
        "model judgment output parser requires tool risk review judgment purpose, got {actual_purpose}"
    )]
    ModelJudgmentPurposeRequired {
        /// Rejected judgment purpose.
        actual_purpose: JudgmentPurpose,
    },

    /// Model judgment output was not one strict JSON object in the expected schema.
    #[error("model judgment output must be one strict JSON object matching the expected schema")]
    InvalidModelJudgmentOutput,

    /// Model judgment output used an unsupported literal field value.
    #[error("model judgment output field {field} must be {expected}, got {actual:?}")]
    InvalidModelJudgmentLiteral {
        /// Name of the invalid model output field.
        field: &'static str,
        /// Expected literal value.
        expected: &'static str,
        /// Rejected value.
        actual: String,
    },

    /// Model judgment output cited evidence that was not supplied by the request.
    #[error("model judgment output evidence index {index} is outside request evidence")]
    ModelJudgmentEvidenceIndexOutOfRange {
        /// Rejected evidence citation index.
        index: usize,
    },

    /// Model judgment output cited the same request evidence more than once.
    #[error("model judgment output evidence index {index} is cited more than once")]
    DuplicateModelJudgmentEvidenceCitation {
        /// Duplicate evidence citation index.
        index: usize,
    },

    /// Model judgment output cited request evidence with the wrong label.
    #[error(
        "model judgment output evidence index {index} label must exactly match request evidence label {expected:?}, got {actual:?}"
    )]
    ModelJudgmentEvidenceLabelMismatch {
        /// Evidence citation index whose label did not match.
        index: usize,
        /// Request-owned evidence label.
        expected: String,
        /// Model-supplied label.
        actual: String,
    },

    /// Model-backed judgment request compilation failed before provider setup.
    #[error("model judgment request could not be compiled ({kind:?}): {message}")]
    ModelJudgmentRequest {
        /// Provider-neutral error category.
        kind: ProviderErrorKind,
        /// Actionable provider-neutral error message.
        message: String,
    },

    /// Model-backed judgment provider setup failed.
    #[error("model judgment provider setup failed ({kind:?}): {message}")]
    ModelJudgmentProviderSetup {
        /// Provider-neutral error category.
        kind: ProviderErrorKind,
        /// Actionable provider-neutral error message.
        message: String,
    },

    /// Model-backed judgment provider stream failed.
    #[error("model judgment provider stream failed ({kind:?}): {message}")]
    ModelJudgmentProviderStream {
        /// Provider-neutral error category.
        kind: ProviderErrorKind,
        /// Actionable provider-neutral error message.
        message: String,
    },

    /// Model-backed judgment response did not match the accepted stream shape.
    #[error("model judgment response shape is unsupported: {reason}")]
    InvalidModelJudgmentResponseShape {
        /// Stable reason for the rejected provider-neutral response shape.
        reason: &'static str,
    },

    /// Internal judgment record id was invalid.
    #[error("judgment record id {value:?} is invalid: {reason}")]
    InvalidRecordId {
        /// Rejected identifier text.
        value: String,
        /// Actionable reason.
        reason: &'static str,
    },

    /// Internal judgment record id already exists.
    #[error("judgment record id {id} is already recorded")]
    DuplicateRecordId {
        /// Duplicate record identifier.
        id: JudgmentRecordId,
    },

    /// Judgment evidence could not be read from the session artifact registry.
    #[error("judgment evidence artifact {artifact_id} is unreadable: {source}")]
    UnreadableEvidence {
        /// Artifact identifier referenced by unreadable evidence.
        artifact_id: ArtifactId,
        /// Artifact registry read/locator failure.
        source: ArtifactError,
    },

    /// Judgment source observed cooperative cancellation before producing output.
    #[error("judgment source cancelled before producing an advisory outcome")]
    Cancelled,
}
