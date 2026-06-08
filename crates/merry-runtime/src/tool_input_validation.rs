//! Runtime-owned JSON Schema validation for model-supplied tool arguments.

use crate::ArtifactContent;
use merry_core::{ErrorInfo, PendingToolCall, ToolInputSchema, ToolName};
use serde_json::{Value, json};
use std::sync::Arc;
use thiserror::Error;

/// Diagnostic code used when model-supplied arguments fail a registered schema.
pub(crate) const DIAGNOSTIC_TOOL_INPUT_SCHEMA_INVALID: &str = "tool_input_schema_invalid";

const TOOL_INPUT_SCHEMA_INVALID_MESSAGE: &str =
    "tool arguments did not match the registered input schema";

/// Precompiled validator for one tool input schema.
#[derive(Clone)]
pub(crate) struct CompiledToolInputValidator {
    validator: Arc<jsonschema::Validator>,
}

impl CompiledToolInputValidator {
    pub(crate) fn compile(schema: &ToolInputSchema) -> Result<Self, ToolInputValidatorError> {
        let validator = jsonschema::options()
            .build(schema.as_schema().as_value())
            .map_err(|source| ToolInputValidatorError::InvalidSchema {
                message: source.to_string(),
            })?;

        Ok(Self {
            validator: Arc::new(validator),
        })
    }

    pub(crate) fn validate_call(
        &self,
        call: &PendingToolCall,
    ) -> Result<(), ToolInputValidationError> {
        let arguments = Value::Object(call.arguments().as_object().clone());
        if self.validator.is_valid(&arguments) {
            return Ok(());
        }

        let violations = self
            .validator
            .iter_errors(&arguments)
            .map(|error| ToolInputViolation {
                path: instance_path(&error),
                schema_path: error.schema_path.to_string(),
                message: error.to_string(),
            })
            .collect();

        Err(ToolInputValidationError { violations })
    }
}

impl std::fmt::Debug for CompiledToolInputValidator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompiledToolInputValidator")
            .finish_non_exhaustive()
    }
}

/// Error raised while compiling a registered input schema.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum ToolInputValidatorError {
    #[error("tool input schema could not be compiled: {message}")]
    InvalidSchema { message: String },
}

/// Validation failure for one model-supplied tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolInputValidationError {
    violations: Vec<ToolInputViolation>,
}

impl ToolInputValidationError {
    pub(crate) fn diagnostic(&self) -> ErrorInfo {
        ErrorInfo::new(
            DIAGNOSTIC_TOOL_INPUT_SCHEMA_INVALID,
            TOOL_INPUT_SCHEMA_INVALID_MESSAGE,
        )
        .expect("static tool input validation diagnostic is valid")
    }

    pub(crate) fn content_for_call(&self, call: &PendingToolCall) -> ArtifactContent {
        ArtifactContent::json(self.payload_for_call(call).to_string())
    }

    pub(crate) fn payload_for_call(&self, call: &PendingToolCall) -> Value {
        schema_failure_payload(call.name(), Some(call.id().as_str()), &self.violations)
    }
}

/// Builds a failed tool-result artifact payload for invalid tool input.
pub(crate) fn schema_failure_payload(
    tool: &ToolName,
    call_id: Option<&str>,
    violations: &[ToolInputViolation],
) -> Value {
    json!({
        "ok": false,
        "tool": tool.as_str(),
        "call_id": call_id,
        "error": {
            "code": DIAGNOSTIC_TOOL_INPUT_SCHEMA_INVALID,
            "message": TOOL_INPUT_SCHEMA_INVALID_MESSAGE,
            "violations": violations.iter().map(ToolInputViolation::as_json).collect::<Vec<_>>(),
        },
        "retry": {
            "instruction": "Call the tool again with arguments matching its JSON schema."
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolInputViolation {
    path: String,
    schema_path: String,
    message: String,
}

impl ToolInputViolation {
    fn as_json(&self) -> Value {
        json!({
            "path": self.path,
            "schema_path": self.schema_path,
            "message": self.message,
        })
    }
}

fn instance_path(error: &jsonschema::ValidationError<'_>) -> String {
    let path = error.instance_path.to_string();
    if path.is_empty() {
        "$".to_owned()
    } else {
        path
    }
}
