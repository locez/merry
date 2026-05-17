//! Tool specification vocabulary.

use crate::{CoreError, ToolInputSchema, ToolName};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

const MAX_TOOL_DESCRIPTION_LEN: usize = 4096;

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
