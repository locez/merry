use merry_core::{CoreError, ToolInputSchema, ToolSpec};
use merry_runtime::ToolBuildError;
use serde_json::Value;

/// Applies a session-stable limit without changing the declared tool identity.
pub(crate) fn with_property_limit(
    spec: ToolSpec,
    property: &str,
    keyword: &str,
    limit: usize,
) -> Result<ToolSpec, ToolBuildError> {
    let mut schema = spec.input_schema().as_schema().as_value().clone();
    let field = schema
        .get_mut("properties")
        .and_then(|properties| properties.get_mut(property))
        .and_then(Value::as_object_mut)
        .ok_or(CoreError::InvalidSchema {
            kind: "ToolInputSchema",
            reason: "bounded tool input must contain an object-valued property",
        })?;
    field.insert(keyword.to_owned(), Value::from(limit));
    let schema = schemars::Schema::try_from(schema).map_err(|_| CoreError::InvalidSchema {
        kind: "ToolInputSchema",
        reason: "bounded tool input schema is invalid",
    })?;
    Ok(spec.with_input_schema(ToolInputSchema::new(schema)?.require_object()?)?)
}
