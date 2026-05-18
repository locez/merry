//! Tool specification and invocation vocabulary.

use crate::{CoreError, ToolCallId, ToolInputSchema, ToolName};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Value};

const MAX_TOOL_DESCRIPTION_LEN: usize = 4096;

/// JSON object arguments supplied with a pending tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ToolCallArguments(Map<String, Value>);

impl ToolCallArguments {
    /// Creates tool call arguments from a JSON object map.
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

impl TryFrom<Value> for ToolCallArguments {
    type Error = CoreError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Object(arguments) => Ok(Self(arguments)),
            _ => Err(CoreError::InvalidToolCall {
                reason: "ToolCallArguments must be a JSON object",
            }),
        }
    }
}

impl From<ToolCallArguments> for Value {
    fn from(arguments: ToolCallArguments) -> Self {
        Value::Object(arguments.into_inner())
    }
}

impl<'de> Deserialize<'de> for ToolCallArguments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::try_from(value).map_err(de::Error::custom)
    }
}

/// Provider-neutral pending tool call requested by a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PendingToolCall {
    /// Provider/model-originated call identifier.
    id: ToolCallId,
    /// Provider-portable tool name.
    name: ToolName,
    /// JSON object arguments supplied for the tool call.
    arguments: ToolCallArguments,
}

impl PendingToolCall {
    /// Creates a pending tool call from validated parts.
    #[must_use]
    pub fn new(id: ToolCallId, name: ToolName, arguments: ToolCallArguments) -> Self {
        Self {
            id,
            name,
            arguments,
        }
    }

    /// Borrows the provider/model-originated call id.
    #[must_use]
    pub fn id(&self) -> &ToolCallId {
        &self.id
    }

    /// Borrows the provider-portable tool name.
    #[must_use]
    pub fn name(&self) -> &ToolName {
        &self.name
    }

    /// Borrows the JSON object arguments.
    #[must_use]
    pub fn arguments(&self) -> &ToolCallArguments {
        &self.arguments
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingToolCallWire {
    id: ToolCallId,
    name: ToolName,
    arguments: ToolCallArguments,
}

impl<'de> Deserialize<'de> for PendingToolCall {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PendingToolCallWire::deserialize(deserializer)?;
        Ok(Self::new(wire.id, wire.name, wire.arguments))
    }
}

/// Provider-neutral tool specification.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// Provider-portable tool name.
    name: ToolName,
    /// Human-readable tool description.
    description: String,
    /// Provider-neutral JSON schema for tool input.
    input_schema: ToolInputSchema,
}

impl ToolSpec {
    /// Creates a validated provider-neutral tool specification.
    pub fn new(
        name: ToolName,
        description: &str,
        input_schema: ToolInputSchema,
    ) -> Result<Self, CoreError> {
        validate_description(description)?;
        Ok(Self {
            name,
            description: description.to_owned(),
            input_schema,
        })
    }

    /// Borrows the provider-portable tool name.
    #[must_use]
    pub fn name(&self) -> &ToolName {
        &self.name
    }

    /// Borrows the human-readable tool description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Borrows the provider-neutral input schema.
    #[must_use]
    pub fn input_schema(&self) -> &ToolInputSchema {
        &self.input_schema
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolSpecWire {
    name: ToolName,
    description: String,
    input_schema: ToolInputSchema,
}

impl<'de> Deserialize<'de> for ToolSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ToolSpecWire::deserialize(deserializer)?;
        Self::new(wire.name, &wire.description, wire.input_schema).map_err(de::Error::custom)
    }
}

fn validate_description(description: &str) -> Result<(), CoreError> {
    if description.trim().is_empty() {
        return Err(CoreError::InvalidToolSpec {
            reason: "ToolSpec description must not be blank",
        });
    }

    if description.chars().count() > MAX_TOOL_DESCRIPTION_LEN {
        return Err(CoreError::InvalidToolSpec {
            reason: "ToolSpec description is longer than the allowed maximum length",
        });
    }

    if description.chars().any(char::is_control) {
        return Err(CoreError::InvalidToolSpec {
            reason: "ToolSpec description must not contain control characters",
        });
    }

    Ok(())
}
