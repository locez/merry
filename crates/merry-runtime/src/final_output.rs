//! Runtime-owned structured final output contract.

use crate::tool_input_validation::{
    CompiledToolInputValidator, ToolInputValidationError, ToolInputValidatorError,
};
use merry_core::{
    ArtifactRef, CoreError, PendingToolCall, ToolCallId, ToolInputSchema, ToolName, ToolSpec,
};
use serde_json::Value;
use thiserror::Error;

/// Reserved provider-visible tool name used for structured terminal output.
pub const FINAL_OUTPUT_TOOL_NAME: &str = "merry_final_output";

/// Runtime-owned final-output contract rendered as a synthetic tool.
#[derive(Debug, Clone)]
pub struct FinalOutputContract {
    tool_spec: ToolSpec,
    input_validator: CompiledToolInputValidator,
}

impl PartialEq for FinalOutputContract {
    fn eq(&self, other: &Self) -> bool {
        self.tool_spec == other.tool_spec
    }
}

impl FinalOutputContract {
    /// Creates a final-output contract from a JSON object schema.
    pub fn new(schema: ToolInputSchema) -> Result<Self, FinalOutputContractError> {
        let value = serde_json::to_value(schema.as_schema()).map_err(|source| {
            FinalOutputContractError::SchemaSerialization {
                message: source.to_string(),
            }
        })?;
        validate_schema_field_descriptions(&value)?;
        let tool_spec = ToolSpec::new(
            ToolName::new(FINAL_OUTPUT_TOOL_NAME)?,
            "Submit the final structured output when the task is complete.",
            schema,
        )?;
        let input_validator = CompiledToolInputValidator::compile(tool_spec.input_schema())?;

        Ok(Self {
            tool_spec,
            input_validator,
        })
    }

    /// Borrows the reserved tool name.
    #[must_use]
    pub fn tool_name(&self) -> &ToolName {
        self.tool_spec.name()
    }

    /// Borrows the provider-visible synthetic tool specification.
    #[must_use]
    pub fn tool_spec(&self) -> &ToolSpec {
        &self.tool_spec
    }

    pub(crate) fn validate_call(
        &self,
        call: &PendingToolCall,
    ) -> Result<(), ToolInputValidationError> {
        self.input_validator.validate_call(call)
    }
}

/// Structured final output recorded by the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalOutput {
    call_id: ToolCallId,
    artifact: ArtifactRef,
    json: String,
}

impl FinalOutput {
    pub(crate) fn new(call_id: ToolCallId, artifact: ArtifactRef, json: String) -> Self {
        Self {
            call_id,
            artifact,
            json,
        }
    }

    /// Borrows the model/provider-originated final-output tool call id.
    #[must_use]
    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }

    /// Borrows the JSON artifact reference containing the final output.
    #[must_use]
    pub fn artifact(&self) -> &ArtifactRef {
        &self.artifact
    }

    /// Borrows the exact JSON payload.
    #[must_use]
    pub fn json(&self) -> &str {
        &self.json
    }
}

/// Errors raised while constructing a final-output contract.
#[derive(Debug, Error)]
pub enum FinalOutputContractError {
    /// Core validation failed.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// Schema serialization failed.
    #[error("final output schema could not be serialized: {message}")]
    SchemaSerialization { message: String },
    /// Schema compilation failed.
    #[error("final output schema could not be compiled: {message}")]
    SchemaCompilation { message: String },
    /// A top-level object field is missing a useful description.
    #[error("final output schema field {field} must include a description")]
    MissingFieldDescription { field: String },
}

impl From<ToolInputValidatorError> for FinalOutputContractError {
    fn from(source: ToolInputValidatorError) -> Self {
        Self::SchemaCompilation {
            message: source.to_string(),
        }
    }
}

fn validate_schema_field_descriptions(value: &Value) -> Result<(), FinalOutputContractError> {
    let Some(properties) = value.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };

    for (field, schema) in properties {
        let has_description = schema
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|description| !description.trim().is_empty());
        if !has_description {
            return Err(FinalOutputContractError::MissingFieldDescription {
                field: field.clone(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::ToolInputSchema;
    use schemars::Schema;
    use serde_json::json;

    fn schema(value: serde_json::Value) -> ToolInputSchema {
        ToolInputSchema::new(Schema::try_from(value).expect("valid schema"))
            .expect("valid tool input schema")
    }

    #[test]
    fn final_output_contract_uses_reserved_provider_portable_tool_name() {
        let contract = FinalOutputContract::new(schema(json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Short final summary."
                }
            },
            "required": ["summary"],
            "additionalProperties": false
        })))
        .expect("contract should build");

        assert_eq!(contract.tool_name().as_str(), "merry_final_output");
        assert_eq!(contract.tool_spec().name().as_str(), "merry_final_output");
        assert!(
            contract
                .tool_spec()
                .description()
                .contains("final structured output")
        );
    }

    #[test]
    fn final_output_contract_rejects_schema_without_field_descriptions() {
        let error = FinalOutputContract::new(schema(json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string" }
            },
            "required": ["summary"],
            "additionalProperties": false
        })))
        .expect_err("schema field descriptions are required");

        assert_eq!(
            error.to_string(),
            "final output schema field summary must include a description"
        );
    }
}
