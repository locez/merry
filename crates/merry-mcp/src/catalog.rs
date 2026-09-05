use crate::{McpError, McpResult, protocol::McpTool};
use merry_core::{
    ExternalToolBinding, SessionToolCatalog, SessionToolCatalogEntry, ToolAdapterId,
    ToolBindingName, ToolInputSchema, ToolSourceFingerprint, ToolSourceId, ToolSpec,
};
use schemars::Schema;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

pub(crate) const MCP_ADAPTER_ID: &str = "mcp";

pub(crate) fn mcp_catalog(catalog: &SessionToolCatalog) -> McpResult<SessionToolCatalog> {
    SessionToolCatalog::new(
        catalog
            .entries()
            .iter()
            .filter(|entry| entry.binding().adapter().as_str() == MCP_ADAPTER_ID)
            .cloned()
            .collect(),
    )
    .map_err(McpError::from)
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
        .filter(|tool| allowed.contains(tool.name.as_str()))
        .collect()
}

pub(crate) fn mcp_tool_to_catalog_entry(
    server_id: &str,
    fingerprint: &ToolSourceFingerprint,
    tool: &McpTool,
    merry_name: merry_core::ToolName,
) -> McpResult<SessionToolCatalogEntry> {
    let spec = mcp_tool_to_spec(server_id, tool, merry_name)?;
    let binding = ExternalToolBinding::new(
        ToolAdapterId::new(MCP_ADAPTER_ID)?,
        ToolSourceId::new(server_id)?,
        ToolBindingName::new(&tool.name)?,
        fingerprint.clone(),
    );
    Ok(SessionToolCatalogEntry::new(spec, binding))
}

fn mcp_tool_to_spec(
    server_id: &str,
    tool: &McpTool,
    merry_name: merry_core::ToolName,
) -> McpResult<ToolSpec> {
    if !tool.input_schema.is_object()
        || tool
            .input_schema
            .get("type")
            .is_some_and(|kind| kind != "object")
    {
        return Err(McpError::InvalidToolSchema {
            server_id: server_id.to_owned(),
            tool_name: tool.name.clone(),
            message: "tool input must be an object schema".to_owned(),
        });
    }
    let normalized_schema = normalize_mcp_input_schema(tool.input_schema.clone());
    jsonschema::options()
        .build(&normalized_schema)
        .map_err(|_| McpError::InvalidToolSchema {
            server_id: server_id.to_owned(),
            tool_name: tool.name.clone(),
            message: "tool schema cannot be compiled".to_owned(),
        })?;
    let schema =
        Schema::try_from(normalized_schema).map_err(|error| McpError::InvalidToolSchema {
            server_id: server_id.to_owned(),
            tool_name: tool.name.clone(),
            message: error.to_string(),
        })?;
    let input_schema =
        ToolInputSchema::new(schema).map_err(|error| McpError::InvalidToolSchema {
            server_id: server_id.to_owned(),
            tool_name: tool.name.clone(),
            message: error.to_string(),
        })?;
    ToolSpec::new(
        merry_name,
        tool.description
            .as_deref()
            .unwrap_or("MCP tool exposed by a configured server"),
        input_schema,
    )
    .map_err(|error| McpError::InvalidToolSchema {
        server_id: server_id.to_owned(),
        tool_name: tool.name.clone(),
        message: error.to_string(),
    })
}

fn normalize_mcp_input_schema(schema: Value) -> Value {
    let mut object = match schema {
        Value::Object(object) => object,
        _ => Map::new(),
    };

    object
        .entry("type")
        .or_insert_with(|| Value::String("object".to_owned()));
    if !matches!(object.get("properties"), Some(Value::Object(_))) {
        object.insert("properties".to_owned(), Value::Object(Map::new()));
    }

    Value::Object(object)
}
