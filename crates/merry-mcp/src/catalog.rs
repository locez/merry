use crate::{McpError, McpResult};
use merry_core::ToolBindingName;
use rmcp::model::Tool as McpTool;
use serde_json::{Map, Value};
use std::{collections::BTreeSet, fmt, sync::Arc};

const DEFAULT_MCP_TOOL_DESCRIPTION: &str = "MCP tool exposed by a configured server";
const MAX_VALIDATION_ERRORS: usize = 5;

/// Validated live MCP tool definition cached per server connection.
#[derive(Clone)]
pub(crate) struct DiscoveredMcpTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_schema: Value,
    validator: Arc<jsonschema::Validator>,
}

impl fmt::Debug for DiscoveredMcpTool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiscoveredMcpTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

impl DiscoveredMcpTool {
    /// Validates candidate JSON object arguments against this tool's compiled
    /// input schema, returning ownership of the object without cloning.
    pub(crate) fn validate_arguments(
        &self,
        arguments: Map<String, Value>,
    ) -> Result<Map<String, Value>, (Map<String, Value>, String)> {
        let value = Value::Object(arguments);
        let errors = self
            .validator
            .iter_errors(&value)
            .take(MAX_VALIDATION_ERRORS)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        let Value::Object(arguments) = value else {
            return Err((Map::new(), "arguments must be a JSON object".to_owned()));
        };
        if errors.is_empty() {
            Ok(arguments)
        } else {
            Err((arguments, errors.join("; ")))
        }
    }

    /// Returns declared top-level JSON schema property names for search matching.
    pub(crate) fn parameter_names(&self) -> Vec<&str> {
        self.input_schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

pub(crate) fn filter_allowed_tools(
    tools: Vec<McpTool>,
    allowlist: Option<&[String]>,
) -> Vec<McpTool> {
    let Some(allowlist) = allowlist else {
        return tools;
    };
    let allowed = allowlist
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    tools
        .into_iter()
        .filter(|tool| allowed.contains(tool.name.as_ref()))
        .collect()
}

pub(crate) fn parse_mcp_tool(server_id: &str, tool: &McpTool) -> McpResult<DiscoveredMcpTool> {
    let validated_name = ToolBindingName::new(tool.name.as_ref())?;
    if tool
        .input_schema
        .get("type")
        .is_some_and(|kind| kind != "object")
    {
        return Err(McpError::InvalidToolSchema {
            server_id: server_id.to_owned(),
            tool_name: validated_name.to_string(),
            message: "tool input must be an object schema".to_owned(),
        });
    }
    let normalized_schema = normalize_mcp_input_schema(tool.input_schema.as_ref().clone());
    let validator = jsonschema::options()
        .build(&normalized_schema)
        .map_err(|_| McpError::InvalidToolSchema {
            server_id: server_id.to_owned(),
            tool_name: validated_name.to_string(),
            message: "tool schema cannot be compiled".to_owned(),
        })?;
    let description = sanitize_mcp_description(tool.description.as_deref());
    Ok(DiscoveredMcpTool {
        name: validated_name.to_string(),
        description,
        input_schema: normalized_schema,
        validator: Arc::new(validator),
    })
}

/// Normalizes remote MCP tool descriptions into clean single-line text without
/// ASCII/Unicode control characters.
fn sanitize_mcp_description(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return DEFAULT_MCP_TOOL_DESCRIPTION.to_owned();
    };
    let mut sanitized = String::with_capacity(raw.len());
    let mut last_was_space = true;
    for ch in raw.chars() {
        if ch.is_control() || ch.is_whitespace() {
            if !last_was_space {
                sanitized.push(' ');
                last_was_space = true;
            }
        } else {
            sanitized.push(ch);
            last_was_space = false;
        }
    }
    while sanitized.ends_with(' ') {
        sanitized.pop();
    }
    if sanitized.is_empty() {
        DEFAULT_MCP_TOOL_DESCRIPTION.to_owned()
    } else {
        sanitized
    }
}

fn normalize_mcp_input_schema(mut object: Map<String, Value>) -> Value {
    object
        .entry("type")
        .or_insert_with(|| Value::String("object".to_owned()));
    if !matches!(object.get("properties"), Some(Value::Object(_))) {
        object.insert("properties".to_owned(), Value::Object(Map::new()));
    }

    Value::Object(object)
}
