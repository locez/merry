//! Model tool call protocol.

use crate::ModelError;
use merry_core::ToolName;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Value};
use std::{fmt, str::FromStr};

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
