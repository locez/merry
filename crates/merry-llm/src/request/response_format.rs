//! Typed response-format and structured-output schema contracts.

use crate::{ModelError, tool::validate_provider_identifier};
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Deserializer, Serialize, de};

/// Provider-neutral contract for model response shape.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelResponseFormat {
    /// Request that the model response adhere to a strict JSON Schema.
    StructuredOutput(ModelStructuredOutputFormat),
}

/// Strict JSON Schema response contract for a model response.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelStructuredOutputFormat {
    name: String,
    schema: Schema,
    strict: bool,
}

impl ModelStructuredOutputFormat {
    /// Creates a strict structured-output response contract.
    pub fn new(name: &str, schema: Schema) -> Result<Self, ModelError> {
        validate_provider_identifier("ModelStructuredOutputFormat name", name)?;
        if schema.as_object().is_none() {
            return Err(ModelError::invalid_request(
                "ModelStructuredOutputFormat schema must be a JSON object",
            ));
        }

        Ok(Self {
            name: name.to_owned(),
            schema,
            strict: true,
        })
    }

    /// Stable schema name sent to the provider.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// JSON Schema the response must satisfy.
    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Whether schema adherence is strict.
    #[must_use]
    pub fn strict(&self) -> bool {
        self.strict
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ModelResponseFormatWire {
    StructuredOutput(ModelStructuredOutputFormatWire),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelStructuredOutputFormatWire {
    name: String,
    schema: Schema,
    strict: bool,
}

impl TryFrom<ModelStructuredOutputFormatWire> for ModelStructuredOutputFormat {
    type Error = ModelError;

    fn try_from(wire: ModelStructuredOutputFormatWire) -> Result<Self, Self::Error> {
        if !wire.strict {
            return Err(ModelError::invalid_request(
                "ModelStructuredOutputFormat strict must be true",
            ));
        }

        Self::new(&wire.name, wire.schema)
    }
}

impl<'de> Deserialize<'de> for ModelResponseFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match ModelResponseFormatWire::deserialize(deserializer)? {
            ModelResponseFormatWire::StructuredOutput(format) => Ok(Self::StructuredOutput(
                format.try_into().map_err(de::Error::custom)?,
            )),
        }
    }
}
