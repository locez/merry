//! Provider-neutral schema wrappers.

use crate::CoreError;
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Deserializer, Serialize, de};

/// JSON object schema accepted as tool input.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ToolInputSchema(Schema);

impl ToolInputSchema {
    /// Creates a tool input schema from a JSON schema object.
    pub fn new(schema: Schema) -> Result<Self, CoreError> {
        if schema.as_object().is_none() {
            return Err(CoreError::InvalidSchema {
                kind: "ToolInputSchema",
                reason: "ToolInputSchema must be a JSON object",
            });
        }

        Ok(Self(schema))
    }

    /// Returns whether the schema describes the JSON object shape accepted by
    /// a model tool call.
    #[must_use]
    pub fn is_object_schema(&self) -> bool {
        self.0
            .as_value()
            .get("type")
            .and_then(serde_json::Value::as_str)
            == Some("object")
    }

    /// Requires the schema to describe a JSON object.
    pub fn require_object(self) -> Result<Self, CoreError> {
        if !self.is_object_schema() {
            return Err(CoreError::InvalidSchema {
                kind: "ToolInputSchema",
                reason: "ToolInputSchema must describe a JSON object",
            });
        }

        Ok(self)
    }

    /// Borrows the wrapped schema.
    #[must_use]
    pub fn as_schema(&self) -> &Schema {
        &self.0
    }

    /// Consumes the wrapper and returns the schema.
    #[must_use]
    pub fn into_schema(self) -> Schema {
        self.0
    }
}

impl<'de> Deserialize<'de> for ToolInputSchema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let schema = Schema::deserialize(deserializer)?;
        Self::new(schema).map_err(de::Error::custom)
    }
}
