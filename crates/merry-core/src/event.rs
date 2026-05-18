//! Runtime event protocol vocabulary.

use crate::{ArtifactRef, CoreError, EvidenceRef, PendingToolCall, SessionId};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

const MAX_DIAGNOSTIC_CODE_LEN: usize = 128;
const MAX_DIAGNOSTIC_MESSAGE_LEN: usize = 4096;

/// Serializable runtime diagnostic.
///
/// This type carries stable diagnostic text only; it does not serialize Rust
/// error internals such as source chains or backtraces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorInfo {
    /// Stable diagnostic code or category.
    code: String,
    /// Human-readable diagnostic message.
    message: String,
}

impl ErrorInfo {
    /// Creates a validated serializable diagnostic.
    pub fn new(code: &str, message: &str) -> Result<Self, CoreError> {
        validate_diagnostic_code(code)?;
        validate_diagnostic_message(message)?;
        Ok(Self {
            code: code.to_owned(),
            message: message.to_owned(),
        })
    }

    /// Borrows the stable diagnostic code or category.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Borrows the human-readable diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorInfoWire {
    code: String,
    message: String,
}

impl<'de> Deserialize<'de> for ErrorInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ErrorInfoWire::deserialize(deserializer)?;
        Self::new(&wire.code, &wire.message).map_err(de::Error::custom)
    }
}

/// Provider-neutral observable runtime event.
///
/// `ArtifactRecorded` and `EvidenceReferenced` events are valid only after the
/// referenced durable state has been recorded. Runtime enforcement is added in a
/// later milestone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEvent {
    /// Session that emitted the event.
    pub session_id: SessionId,
    /// Monotonic event sequence within the session.
    pub sequence: u64,
    /// Event payload.
    pub kind: RuntimeEventKind,
}

impl RuntimeEvent {
    /// Creates a runtime event.
    #[must_use]
    pub fn new(session_id: SessionId, sequence: u64, kind: RuntimeEventKind) -> Self {
        Self {
            session_id,
            sequence,
            kind,
        }
    }
}

/// Provider-neutral runtime event variants.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeEventKind {
    /// A session was initialized.
    SessionStarted,
    /// A runtime step started.
    StepStarted,
    /// A runtime step completed.
    StepCompleted,
    /// An artifact reference was recorded.
    ArtifactRecorded { artifact: ArtifactRef },
    /// Exact evidence was referenced.
    EvidenceReferenced { evidence: EvidenceRef },
    /// A model requested a tool call that is waiting for runtime policy/execution.
    ToolCallPending { call: PendingToolCall },
    /// The runtime was cancelled.
    Cancelled { diagnostic: ErrorInfo },
    /// The runtime failed.
    Failed { diagnostic: ErrorInfo },
}

fn validate_diagnostic_code(code: &str) -> Result<(), CoreError> {
    if code.trim().is_empty() {
        return Err(invalid_diagnostic(
            "ErrorInfo code",
            code,
            "must not be blank",
        ));
    }

    if code.trim() != code {
        return Err(invalid_diagnostic(
            "ErrorInfo code",
            code,
            "must not have leading or trailing whitespace",
        ));
    }

    if code.chars().count() > MAX_DIAGNOSTIC_CODE_LEN {
        return Err(invalid_diagnostic(
            "ErrorInfo code",
            code,
            "is longer than the allowed maximum length",
        ));
    }

    if code.chars().any(char::is_control) {
        return Err(invalid_diagnostic(
            "ErrorInfo code",
            code,
            "must not contain control characters",
        ));
    }

    Ok(())
}

fn validate_diagnostic_message(message: &str) -> Result<(), CoreError> {
    if message.trim().is_empty() {
        return Err(invalid_diagnostic(
            "ErrorInfo message",
            message,
            "must not be blank",
        ));
    }

    if message.chars().count() > MAX_DIAGNOSTIC_MESSAGE_LEN {
        return Err(invalid_diagnostic(
            "ErrorInfo message",
            message,
            "is longer than the allowed maximum length",
        ));
    }

    if message.chars().any(char::is_control) {
        return Err(invalid_diagnostic(
            "ErrorInfo message",
            message,
            "must not contain control characters",
        ));
    }

    Ok(())
}

fn invalid_diagnostic(kind: &'static str, value: &str, reason: &'static str) -> CoreError {
    CoreError::InvalidIdentifier {
        kind,
        value: value.to_owned(),
        reason,
    }
}
