//! Runtime event protocol vocabulary.

use crate::CoreError;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeMap;

const MAX_DIAGNOSTIC_CODE_LEN: usize = 128;
const MAX_DIAGNOSTIC_MESSAGE_LEN: usize = 4096;
const MAX_ERROR_HINT_LEN: usize = 512;
const MAX_ERROR_CONTEXT_VALUE_LEN: usize = 512;

const ALLOWED_ERROR_CONTEXT_KEYS: &[&str] = &[
    "session_id",
    "turn_id",
    "call_id",
    "tool_name",
    "provider_name",
    "model_role",
    "config_path",
    "field_path",
    "artifact_id",
    "checkpoint_id",
    "http_status",
    "exit_code",
];

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

/// Stable SDK-facing error domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MerryErrorDomain {
    /// Configuration loading, parsing, or validation failed.
    Config,
    /// A model/provider adapter failed.
    Provider,
    /// Runtime orchestration failed.
    Runtime,
    /// Tool declaration, dispatch, or execution failed.
    Tool,
    /// A runtime policy rejected or blocked work.
    Policy,
    /// Context compilation or context budget handling failed.
    Context,
    /// Compaction failed or produced invalid state.
    Compaction,
    /// Artifact persistence or lookup failed.
    Artifact,
    /// Sandbox execution or permission handling failed.
    Sandbox,
    /// An internal invariant failed.
    Internal,
}

/// Stable SDK-facing retry guidance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MerryRetryability {
    /// The operation may succeed if retried without user changes.
    Retryable,
    /// Retrying the same operation is not expected to help.
    NotRetryable,
    /// The user must change input, configuration, or permissions before retrying.
    UserActionRequired,
    /// The operation stopped because cancellation was requested or observed.
    Cancelled,
    /// Retry behavior is unknown.
    Unknown,
}

/// Stable SDK-facing diagnostic metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MerryErrorInfo {
    /// Stable SDK-facing diagnostic code.
    code: String,
    /// Provider-neutral error domain.
    domain: MerryErrorDomain,
    /// Human-readable diagnostic message.
    message: String,
    /// Optional actionable hint.
    hint: Option<String>,
    /// Provider-neutral retry guidance.
    retryability: MerryRetryability,
    /// Bounded non-sensitive context values.
    context: BTreeMap<String, String>,
}

impl MerryErrorInfo {
    /// Starts building stable SDK-facing diagnostic metadata.
    #[must_use]
    pub fn builder(
        code: &str,
        domain: MerryErrorDomain,
        message: &str,
        retryability: MerryRetryability,
    ) -> MerryErrorInfoBuilder {
        MerryErrorInfoBuilder {
            code: code.to_owned(),
            domain,
            message: message.to_owned(),
            hint: None,
            retryability,
            context: BTreeMap::new(),
        }
    }

    /// Borrows the stable SDK-facing diagnostic code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Borrows the provider-neutral error domain.
    #[must_use]
    pub fn domain(&self) -> &MerryErrorDomain {
        &self.domain
    }

    /// Borrows the human-readable diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Borrows the optional actionable hint.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    /// Borrows the provider-neutral retry guidance.
    #[must_use]
    pub fn retryability(&self) -> &MerryRetryability {
        &self.retryability
    }

    /// Borrows the bounded non-sensitive context values.
    #[must_use]
    pub fn context(&self) -> &BTreeMap<String, String> {
        &self.context
    }
}

/// Builder for stable SDK-facing diagnostic metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerryErrorInfoBuilder {
    code: String,
    domain: MerryErrorDomain,
    message: String,
    hint: Option<String>,
    retryability: MerryRetryability,
    context: BTreeMap<String, String>,
}

impl MerryErrorInfoBuilder {
    /// Adds an actionable hint.
    #[must_use]
    pub fn hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.to_owned());
        self
    }

    /// Adds bounded non-sensitive context metadata.
    #[must_use]
    pub fn context(mut self, key: &str, value: &str) -> Self {
        self.context.insert(key.to_owned(), value.to_owned());
        self
    }

    /// Builds validated SDK-facing diagnostic metadata.
    pub fn build(self) -> Result<MerryErrorInfo, CoreError> {
        validate_diagnostic_code(&self.code)?;
        validate_diagnostic_message(&self.message)?;
        validate_error_hint(self.hint.as_deref())?;
        validate_error_context(&self.context)?;

        Ok(MerryErrorInfo {
            code: self.code,
            domain: self.domain,
            message: self.message,
            hint: self.hint,
            retryability: self.retryability,
            context: self.context,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MerryErrorInfoWire {
    code: String,
    domain: MerryErrorDomain,
    message: String,
    hint: Option<String>,
    retryability: MerryRetryability,
    context: BTreeMap<String, String>,
}

impl<'de> Deserialize<'de> for MerryErrorInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = MerryErrorInfoWire::deserialize(deserializer)?;
        MerryErrorInfo::builder(&wire.code, wire.domain, &wire.message, wire.retryability)
            .maybe_hint(wire.hint)
            .context_map(wire.context)
            .build()
            .map_err(de::Error::custom)
    }
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

fn validate_error_hint(hint: Option<&str>) -> Result<(), CoreError> {
    let Some(hint) = hint else {
        return Ok(());
    };

    if hint.trim().is_empty() {
        return Err(invalid_diagnostic(
            "MerryErrorInfo hint",
            hint,
            "must not be blank",
        ));
    }

    if hint.chars().count() > MAX_ERROR_HINT_LEN {
        return Err(invalid_diagnostic(
            "MerryErrorInfo hint",
            hint,
            "is longer than the allowed maximum length",
        ));
    }

    if hint.chars().any(char::is_control) {
        return Err(invalid_diagnostic(
            "MerryErrorInfo hint",
            hint,
            "must not contain control characters",
        ));
    }

    Ok(())
}

fn validate_error_context(context: &BTreeMap<String, String>) -> Result<(), CoreError> {
    for (key, value) in context {
        if !ALLOWED_ERROR_CONTEXT_KEYS.contains(&key.as_str()) {
            return Err(invalid_diagnostic(
                "MerryErrorInfo context",
                key,
                "context key is not allowed",
            ));
        }

        if value.chars().count() > MAX_ERROR_CONTEXT_VALUE_LEN {
            return Err(invalid_diagnostic(
                "MerryErrorInfo context value",
                value,
                "is longer than the allowed maximum length",
            ));
        }

        if value.chars().any(char::is_control) {
            return Err(invalid_diagnostic(
                "MerryErrorInfo context value",
                value,
                "must not contain control characters",
            ));
        }
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

impl MerryErrorInfoBuilder {
    fn maybe_hint(mut self, hint: Option<String>) -> Self {
        self.hint = hint;
        self
    }

    fn context_map(mut self, context: BTreeMap<String, String>) -> Self {
        self.context = context;
        self
    }
}
