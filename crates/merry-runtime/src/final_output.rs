//! Runtime-owned structured final output contract.

use crate::tool_input_validation::{
    CompiledToolInputValidator, ToolInputValidationError, ToolInputValidatorError,
};
use merry_core::{
    ArtifactRef, CoreError, PendingToolCall, ToolCallId, ToolInputSchema, ToolName, ToolSpec,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{collections::BTreeSet, fmt, sync::Arc};
use thiserror::Error;

type OutputValidator = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// Reserved provider-visible tool name used for structured terminal output.
pub const FINAL_OUTPUT_TOOL_NAME: &str = "merry_final_output";

/// Runtime-owned final-output contract rendered as a synthetic tool.
#[derive(Clone)]
pub struct FinalOutputContract {
    tool_spec: ToolSpec,
    input_validator: CompiledToolInputValidator,
    output_validator: Option<OutputValidator>,
}

impl fmt::Debug for FinalOutputContract {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FinalOutputContract")
            .field("tool_spec", &self.tool_spec)
            .field("input_validator", &self.input_validator)
            .field("output_validator", &self.output_validator.is_some())
            .finish()
    }
}

impl PartialEq for FinalOutputContract {
    fn eq(&self, other: &Self) -> bool {
        self.tool_spec == other.tool_spec
    }
}

impl FinalOutputContract {
    /// Creates a final-output contract from a JSON object schema.
    pub fn new(schema: ToolInputSchema) -> Result<Self, FinalOutputContractError> {
        if !schema.is_object_schema() {
            return Err(FinalOutputContractError::RootSchemaMustBeObject);
        }
        let schema = schema.require_object()?;
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
            output_validator: None,
        })
    }

    /// Adds a typed output decoder used before the final output is recorded.
    ///
    /// Runtime still records the raw JSON as the authoritative final-output
    /// artifact. The decoder only provides an additional application-level
    /// validity check so a structured-output retry can happen inside the same
    /// runtime loop and session.
    #[must_use]
    pub fn with_output_decoder<T>(mut self) -> Self
    where
        T: DeserializeOwned,
    {
        self.output_validator = Some(Arc::new(|json| {
            serde_json::from_str::<T>(json)
                .map(|_| ())
                .map_err(|source| source.to_string())
        }));
        self
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

    pub(crate) fn validate_output(&self, json: &str) -> Result<(), String> {
        self.output_validator
            .as_ref()
            .map_or(Ok(()), |validator| validator(json))
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
    /// Structured final output is represented by one provider-visible object
    /// tool call, so scalar and array root schemas are not supported.
    #[error("final output schema root must describe a JSON object")]
    RootSchemaMustBeObject,
    /// An object field is missing a useful provider-facing description.
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
    fn walk(
        root: &Value,
        schema: &Value,
        path: &str,
        visited_refs: &mut BTreeSet<String>,
    ) -> Result<(), FinalOutputContractError> {
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
            && visited_refs.insert(reference.to_owned())
        {
            let target = reference
                .strip_prefix('#')
                .and_then(|pointer| root.pointer(pointer))
                .ok_or_else(|| FinalOutputContractError::MissingFieldDescription {
                    field: format!("{path} ({reference})"),
                })?;
            walk(root, target, path, visited_refs)?;
        }

        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (field, field_schema) in properties {
                if field == "type" {
                    continue;
                }
                let field_path = format!("{path}.{field}");
                let has_description = field_schema
                    .get("description")
                    .and_then(Value::as_str)
                    .is_some_and(|description| !description.trim().is_empty());
                if !has_description {
                    return Err(FinalOutputContractError::MissingFieldDescription {
                        field: field_path
                            .strip_prefix("$.")
                            .unwrap_or(&field_path)
                            .to_owned(),
                    });
                }
                walk(root, field_schema, &field_path, visited_refs)?;
            }
        }

        if let Some(items) = schema.get("items") {
            walk(root, items, &format!("{path}[]"), visited_refs)?;
        }

        for keyword in ["oneOf", "anyOf", "allOf"] {
            if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
                for (index, branch) in branches.iter().enumerate() {
                    walk(
                        root,
                        branch,
                        &format!("{path}.{keyword}[{index}]"),
                        visited_refs,
                    )?;
                }
            }
        }

        if let Some(definitions) = schema.get("$defs").and_then(Value::as_object) {
            for (name, definition) in definitions {
                walk(
                    root,
                    definition,
                    &format!("{path}.$defs.{name}"),
                    visited_refs,
                )?;
            }
        }

        Ok(())
    }

    walk(value, value, "$", &mut BTreeSet::new())
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

    #[test]
    fn final_output_contract_rejects_nested_field_without_description() {
        let error = FinalOutputContract::new(schema(json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "object",
                    "description": "Structured final summary.",
                    "properties": {
                        "status": { "type": "string" }
                    }
                }
            },
            "required": ["summary"],
            "additionalProperties": false
        })))
        .expect_err("nested schema field descriptions are required");

        assert_eq!(
            error.to_string(),
            "final output schema field summary.status must include a description"
        );
    }

    #[test]
    fn final_output_contract_rejects_scalar_root_schema_explicitly() {
        let error = FinalOutputContract::new(schema(json!({"type": "string"})))
            .expect_err("structured final output must use an object schema");

        assert!(matches!(
            error,
            FinalOutputContractError::RootSchemaMustBeObject
        ));
    }
}
