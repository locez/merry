//! Typed errors for Merry core protocol validation.

use thiserror::Error;

/// Errors raised while constructing or decoding core protocol types.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoreError {
    /// A typed identifier or protocol name failed validation.
    #[error("invalid identifier/name: {kind} {reason} (value: {value:?})")]
    InvalidIdentifier {
        /// The identifier type being validated.
        kind: &'static str,
        /// The invalid value.
        value: String,
        /// Actionable validation detail.
        reason: &'static str,
    },

    /// A provider-neutral schema failed validation.
    #[error("invalid schema for {kind}: {reason}")]
    InvalidSchema {
        /// The schema wrapper being validated.
        kind: &'static str,
        /// Actionable validation detail.
        reason: &'static str,
    },

    /// An evidence locator failed validation.
    #[error("invalid evidence locator for {kind}: {reason} (value: {value:?})")]
    InvalidEvidenceLocator {
        /// The locator field or variant being validated.
        kind: &'static str,
        /// The invalid value.
        value: String,
        /// Actionable validation detail.
        reason: &'static str,
    },

    /// A tool specification failed validation.
    #[error("invalid tool spec: {reason}")]
    InvalidToolSpec {
        /// Actionable validation detail.
        reason: &'static str,
    },
}
