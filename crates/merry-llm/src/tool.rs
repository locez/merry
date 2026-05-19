//! Model tool call protocol.

use crate::ModelError;
use merry_core::{ErrorInfo, ToolCallResultStatus, ToolName};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Value};
use std::{borrow::Cow, fmt, str::FromStr};

const MAX_PROVIDER_IDENTIFIER_LEN: usize = 256;

/// Provider-originated tool call identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ModelToolCallId(String);

impl ModelToolCallId {
    /// Creates a validated model tool call identifier.
    pub fn new(value: &str) -> Result<Self, ModelError> {
        validate_provider_identifier("ModelToolCallId", value)?;
        Ok(Self(value.to_owned()))
    }

    /// Borrows the identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelToolCallId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ModelToolCallId {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ModelToolCallId {
    type Error = ModelError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for ModelToolCallId {
    type Error = ModelError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_provider_identifier("ModelToolCallId", &value)?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for ModelToolCallId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(de::Error::custom)
    }
}

/// JSON object arguments supplied with a model-requested tool call.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ToolArguments(Map<String, Value>);

impl ToolArguments {
    /// Creates tool arguments from a JSON object map.
    #[must_use]
    pub fn new(arguments: Map<String, Value>) -> Self {
        Self(arguments)
    }

    /// Borrows the wrapped JSON object.
    #[must_use]
    pub fn as_object(&self) -> &Map<String, Value> {
        &self.0
    }

    /// Consumes the wrapper and returns the JSON object.
    #[must_use]
    pub fn into_inner(self) -> Map<String, Value> {
        self.0
    }
}

impl TryFrom<Value> for ToolArguments {
    type Error = ModelError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Object(arguments) => Ok(Self(arguments)),
            _ => Err(ModelError::invalid_request(
                "ToolArguments must be a JSON object",
            )),
        }
    }
}

impl From<ToolArguments> for Value {
    fn from(arguments: ToolArguments) -> Self {
        Value::Object(arguments.into_inner())
    }
}

impl<'de> Deserialize<'de> for ToolArguments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::try_from(value).map_err(de::Error::custom)
    }
}

/// Provider-neutral tool call requested by a model.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelToolCall {
    id: ModelToolCallId,
    name: ToolName,
    arguments: ToolArguments,
}

impl ModelToolCall {
    /// Creates a model tool call from validated parts.
    #[must_use]
    pub fn new(id: ModelToolCallId, name: ToolName, arguments: ToolArguments) -> Self {
        Self {
            id,
            name,
            arguments,
        }
    }

    /// Borrows the provider-originated call id.
    #[must_use]
    pub fn id(&self) -> &ModelToolCallId {
        &self.id
    }

    /// Borrows the Merry tool name.
    #[must_use]
    pub fn name(&self) -> &ToolName {
        &self.name
    }

    /// Borrows the JSON object arguments.
    #[must_use]
    pub fn arguments(&self) -> &ToolArguments {
        &self.arguments
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelToolCallWire {
    id: ModelToolCallId,
    name: ToolName,
    arguments: ToolArguments,
}

impl<'de> Deserialize<'de> for ModelToolCall {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelToolCallWire::deserialize(deserializer)?;
        Ok(Self::new(wire.id, wire.name, wire.arguments))
    }
}

/// Provider-neutral inline content returned by a completed tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelToolResultContent {
    /// Text result content.
    Text(String),
    /// JSON result content encoded as a string.
    Json(String),
}

impl ModelToolResultContent {
    /// Creates validated text result content.
    pub fn text(text: &str) -> Result<Self, ModelError> {
        validate_tool_result_content("ModelToolResultContent text", text)?;
        Ok(Self::Text(text.to_owned()))
    }

    /// Creates validated JSON result content.
    pub fn json(json: &str) -> Result<Self, ModelError> {
        validate_tool_result_content("ModelToolResultContent json", json)?;
        Ok(Self::Json(json.to_owned()))
    }

    /// Returns the content as text when this is text content.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Json(_) => None,
        }
    }

    /// Returns the content as JSON when this is JSON content.
    #[must_use]
    pub fn as_json(&self) -> Option<&str> {
        match self {
            Self::Text(_) => None,
            Self::Json(json) => Some(json),
        }
    }

    /// Borrows the content string regardless of content kind.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Json(json) => json,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ModelToolResultContentRef<'a> {
    Text { text: &'a str },
    Json { json: &'a str },
}

impl Serialize for ModelToolResultContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Text(text) => ModelToolResultContentRef::Text { text }.serialize(serializer),
            Self::Json(json) => ModelToolResultContentRef::Json { json }.serialize(serializer),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ModelToolResultContentWire {
    Text { text: String },
    Json { json: String },
}

impl<'de> Deserialize<'de> for ModelToolResultContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match ModelToolResultContentWire::deserialize(deserializer)? {
            ModelToolResultContentWire::Text { text } => {
                Self::text(&text).map_err(de::Error::custom)
            }
            ModelToolResultContentWire::Json { json } => {
                Self::json(&json).map_err(de::Error::custom)
            }
        }
    }
}

impl JsonSchema for ModelToolResultContent {
    fn schema_name() -> Cow<'static, str> {
        "ModelToolResultContent".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::ModelToolResultContent").into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        ModelToolResultContentWire::json_schema(generator)
    }
}

/// Provider-neutral result for a model-requested tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelToolResult {
    call_id: ModelToolCallId,
    status: ToolCallResultStatus,
    content: ModelToolResultContent,
    diagnostic: Option<ErrorInfo>,
}

impl ModelToolResult {
    /// Creates a validated tool call result.
    pub fn new(
        call_id: ModelToolCallId,
        status: ToolCallResultStatus,
        content: ModelToolResultContent,
        diagnostic: Option<ErrorInfo>,
    ) -> Result<Self, ModelError> {
        validate_result_diagnostic(status, diagnostic.as_ref())?;
        Ok(Self {
            call_id,
            status,
            content,
            diagnostic,
        })
    }

    /// Creates a successful tool call result.
    #[must_use]
    pub fn succeeded(call_id: ModelToolCallId, content: ModelToolResultContent) -> Self {
        Self {
            call_id,
            status: ToolCallResultStatus::Succeeded,
            content,
            diagnostic: None,
        }
    }

    /// Creates a failed tool call result with a diagnostic.
    #[must_use]
    pub fn failed(
        call_id: ModelToolCallId,
        content: ModelToolResultContent,
        diagnostic: ErrorInfo,
    ) -> Self {
        Self {
            call_id,
            status: ToolCallResultStatus::Failed,
            content,
            diagnostic: Some(diagnostic),
        }
    }

    /// Borrows the provider-originated call id being resolved.
    #[must_use]
    pub fn call_id(&self) -> &ModelToolCallId {
        &self.call_id
    }

    /// Returns the tool call result status.
    #[must_use]
    pub fn status(&self) -> ToolCallResultStatus {
        self.status
    }

    /// Borrows the provider-neutral result content.
    #[must_use]
    pub fn content(&self) -> &ModelToolResultContent {
        &self.content
    }

    /// Borrows the optional failure diagnostic.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&ErrorInfo> {
        self.diagnostic.as_ref()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelToolResultWire {
    call_id: ModelToolCallId,
    status: ToolCallResultStatus,
    content: ModelToolResultContent,
    diagnostic: Option<ErrorInfo>,
}

impl<'de> Deserialize<'de> for ModelToolResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelToolResultWire::deserialize(deserializer)?;
        Self::new(wire.call_id, wire.status, wire.content, wire.diagnostic)
            .map_err(de::Error::custom)
    }
}

/// Ordered model-visible continuation for a tool call and its result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelToolContinuation {
    call: ModelToolCall,
    result: ModelToolResult,
}

impl ModelToolContinuation {
    /// Creates a validated tool continuation.
    pub fn new(call: ModelToolCall, result: ModelToolResult) -> Result<Self, ModelError> {
        if call.id() != result.call_id() {
            return Err(ModelError::invalid_request(
                "ModelToolContinuation call id must match result call_id",
            ));
        }

        Ok(Self { call, result })
    }

    /// Borrows the original model-requested tool call.
    #[must_use]
    pub fn call(&self) -> &ModelToolCall {
        &self.call
    }

    /// Borrows the tool call result.
    #[must_use]
    pub fn result(&self) -> &ModelToolResult {
        &self.result
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelToolContinuationWire {
    call: ModelToolCall,
    result: ModelToolResult,
}

impl<'de> Deserialize<'de> for ModelToolContinuation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelToolContinuationWire::deserialize(deserializer)?;
        Self::new(wire.call, wire.result).map_err(de::Error::custom)
    }
}

pub(crate) fn validate_provider_identifier(
    kind: &'static str,
    value: &str,
) -> Result<(), ModelError> {
    if value.is_empty() {
        return Err(invalid_identifier(kind, "must not be empty"));
    }

    if value.trim().is_empty() {
        return Err(invalid_identifier(kind, "must not be whitespace only"));
    }

    if value.trim() != value {
        return Err(invalid_identifier(
            kind,
            "must not have leading or trailing whitespace",
        ));
    }

    if value.chars().count() > MAX_PROVIDER_IDENTIFIER_LEN {
        return Err(invalid_identifier(
            kind,
            "is longer than the allowed maximum length",
        ));
    }

    if value.chars().any(char::is_control) {
        return Err(invalid_identifier(
            kind,
            "must not contain control characters",
        ));
    }

    Ok(())
}

fn invalid_identifier(kind: &'static str, reason: &'static str) -> ModelError {
    ModelError::invalid_request(format!("{kind} {reason}"))
}

fn validate_tool_result_content(kind: &'static str, value: &str) -> Result<(), ModelError> {
    if value.trim().is_empty() {
        return Err(ModelError::invalid_request(format!(
            "{kind} must not be blank"
        )));
    }

    Ok(())
}

fn validate_result_diagnostic(
    status: ToolCallResultStatus,
    diagnostic: Option<&ErrorInfo>,
) -> Result<(), ModelError> {
    match (status, diagnostic) {
        (ToolCallResultStatus::Succeeded, None) | (ToolCallResultStatus::Failed, Some(_)) => Ok(()),
        (ToolCallResultStatus::Succeeded, Some(_)) => Err(ModelError::invalid_request(
            "succeeded model tool result must not include a diagnostic",
        )),
        (ToolCallResultStatus::Failed, None) => Err(ModelError::invalid_request(
            "failed model tool result must include a diagnostic",
        )),
    }
}
